// Rust guideline compliant 2026-10-31
//! The owner-scoped persistent PTY registry (the dsh `TerminalSessionService`,
//! `packages/terminal/terminal/src/index.ts`).
//!
//! Backends own terminal mechanics while the registry owns ids, publication,
//! authorization, and awaited cleanup: backend types register into an
//! insertion-order table, sessions mint `pty-<n>` ids, every session is
//! scoped to one owner, and spawn runs behind the strict fence sequence
//! (active check, owner liveness, backend lookup, name reservation, pending
//! reservation, backend setup, disposing/owner re-check, publish-or-rollback).
//!
//! Synchronous-port deviations, all recorded against the dsh source:
//! - The dsh `AbortController` interrupts an in-flight backend setup. The fr
//!   backend spawn is synchronous, so [`TerminalRegistry::dispose_all`] and
//!   [`TerminalRegistry::dispose_owned`] wait for the pending-spawn fence to
//!   settle on a condvar; a late publication is rejected by the
//!   disposing/owner re-checks and rolled back. The backend contract bounds
//!   its own setup (the session startup-readiness timeout).
//! - The dsh backend spawn receives an abort signal. [`BackendSpawnSpec`]
//!   carries none: a synchronous backend could not honor it.
//! - The dsh per-owner cleanup effects run inside `disposeAll`. The fr owner
//!   teardown calls [`TerminalRegistry::dispose_owned`] through the agent
//!   service wiring (Phase 16e batch 3), which is observably identical: at
//!   `disposeAll` time those cleanups are no-ops because sessions and pending
//!   spawns are already swept.
//!
//! [`TerminalRegistry::has_owner_activity`] spans the entire spawn-to-close
//! interval with no publication gap; it is the enforcement criterion for the
//! sandbox-mode fence wired in batch 3.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::terminal_session::{CloseSlot, ReadPage, SendOperation, SessionStatus, wait_close};

/// The machine-routable PTY service failures (the dsh `TerminalErrorCode`;
/// the variants serialize verbatim over the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalErrorCode {
    /// A backend of the requested type is already registered.
    DuplicateBackend,
    /// The requested owner-local session name is taken.
    DuplicateName,
    /// The session belongs to a different owner.
    ForeignSession,
    /// No backend is registered for the requested type.
    NoBackend,
    /// The session id is unknown to the registry.
    NoSession,
    /// The owning agent is not live.
    OwnerNotLive,
    /// The session already carries an interactive send.
    SendActive,
    /// The service is disposing.
    ServiceDisposing,
}

impl TerminalErrorCode {
    /// The verbatim wire code.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DuplicateBackend => "DUPLICATE_BACKEND",
            Self::DuplicateName => "DUPLICATE_NAME",
            Self::ForeignSession => "FOREIGN_SESSION",
            Self::NoBackend => "NO_BACKEND",
            Self::NoSession => "NO_SESSION",
            Self::OwnerNotLive => "OWNER_NOT_LIVE",
            Self::SendActive => "SEND_ACTIVE",
            Self::ServiceDisposing => "SERVICE_DISPOSING",
        }
    }
}

/// One registry failure.
///
/// The dsh `TerminalError`, plain errors, and `AggregateError` fold into one
/// shape: a coded failure carries a code, a plain failure carries none, and
/// an aggregated failure lists its causes.
#[derive(Debug, Clone)]
pub struct TerminalFailure {
    /// The failure text.
    pub message: String,
    /// The machine-routable code, absent for plain failures.
    pub code: Option<TerminalErrorCode>,
    /// The nested failure texts of an aggregated failure.
    pub causes: Vec<String>,
}

impl TerminalFailure {
    /// One coded failure.
    #[must_use]
    pub const fn terminal(code: TerminalErrorCode, message: String) -> Self {
        Self {
            message,
            code: Some(code),
            causes: Vec::new(),
        }
    }

    /// One uncoded failure (the dsh plain `Error`).
    #[must_use]
    pub const fn plain(message: String) -> Self {
        Self {
            message,
            code: None,
            causes: Vec::new(),
        }
    }

    /// One aggregated failure (the dsh `AggregateError`).
    #[must_use]
    pub fn aggregate(message: &str, causes: Vec<String>) -> Self {
        Self {
            message: message.to_owned(),
            code: None,
            causes,
        }
    }
}

impl fmt::Display for TerminalFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The spawn request for one owner-scoped session (the dsh
/// `TerminalSpawnRequest`).
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    /// The backend type to spawn under.
    pub backend_type: String,
    /// The optional owner-local session name.
    pub name: Option<String>,
    /// The optional working directory.
    pub cwd: Option<String>,
}

/// One published PTY session snapshot (the dsh `TerminalSessionSnapshot`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSessionSnapshot {
    /// The registry-minted session id.
    pub session_id: String,
    /// The owner-local name, when the session carries one.
    pub name: Option<String>,
    /// The backend type the session spawned under.
    pub backend_type: String,
    /// The provider process id, when the backend reports one.
    pub pid: Option<u32>,
    /// The top-level session status.
    pub status: SessionStatus,
}

/// The spawn outcome for one newly published session (the dsh
/// `TerminalSpawnResult`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSpawnResult {
    /// The registry-minted session id.
    pub session_id: String,
    /// The owner-local name, when the request carried one.
    pub name: Option<String>,
    /// The backend type the session spawned under.
    pub backend_type: String,
    /// The provider process id, when the backend reports one.
    pub pid: Option<u32>,
    /// The top-level session status.
    pub status: SessionStatus,
    /// The captured startup output.
    pub motd: String,
}

/// One backend-session error carrying the cleanup secondary error (the dsh
/// `TerminalBackendCleanupError` folds into the same shape).
#[derive(Debug, Clone)]
pub struct BackendSpawnError {
    /// The setup failure text.
    pub message: String,
    /// The cleanup failure text, when the backend's own cleanup also failed.
    pub cleanup: Option<String>,
}

/// The backend-facing spawn request (the dsh `TerminalBackendSpawnSpec`;
/// the dsh abort-signal field is absent — see the module deviations).
#[derive(Debug, Clone)]
pub struct BackendSpawnSpec {
    /// The registry-minted session id.
    pub session_id: String,
    /// The owning agent id.
    pub owner: String,
    /// The backend type the session spawns under.
    pub backend_type: String,
    /// The optional owner-local session name.
    pub name: Option<String>,
    /// The optional working directory.
    pub cwd: Option<String>,
}

/// One replaceable PTY backend (the dsh `TerminalBackend`).
pub trait TerminalBackend: Send + Sync {
    /// The unique non-empty backend type.
    #[must_use]
    fn backend_type(&self) -> &str;

    /// Creates one session and completes its startup sequence.
    ///
    /// # Errors
    ///
    /// Returns the setup failure, optionally carrying the cleanup secondary
    /// error.
    fn spawn(&self, spec: &BackendSpawnSpec) -> Result<Arc<dyn BackendSession>, BackendSpawnError>;
}

/// One backend-owned live session (the dsh `TerminalBackendSession`).
pub trait BackendSession: Send + Sync {
    /// The provider process id, when the backend reports one.
    #[must_use]
    fn pid(&self) -> Option<u32>;

    /// The top-level session status.
    #[must_use]
    fn status(&self) -> SessionStatus;

    /// The captured startup output.
    #[must_use]
    fn motd(&self) -> String;

    /// One bounded scrollback page.
    ///
    /// # Errors
    ///
    /// Returns the backend's read validation text.
    fn read(&self, offset: Option<usize>, count: Option<usize>) -> Result<ReadPage, String>;

    /// Delivers one allowed signal to the foreground group.
    ///
    /// # Errors
    ///
    /// Returns the backend's delivery failure text.
    fn signal(&self, name: &str) -> Result<i64, String>;

    /// Starts one exclusive interactive send.
    ///
    /// # Errors
    ///
    /// Returns the backend's rejection text.
    fn start_send(self: Arc<Self>, text: &str, submit: bool) -> Result<Arc<SendOperation>, String>;

    /// Closes the session idempotently behind its fence.
    ///
    /// Returns the fence promptly; the settle rides the slot, so a slow
    /// backend close never stalls registry callers under the state lock.
    fn close(self: Arc<Self>, reason: &str) -> Arc<CloseSlot>;
}

/// One published session row.
struct SessionRecord {
    id: String,
    owner: String,
    name: Option<String>,
    backend_type: String,
    session: Arc<dyn BackendSession>,
    active: Option<Arc<SendOperation>>,
    closing: Option<Arc<CloseSlot>>,
}

/// One close sweep target (the session id, session, and live close fence).
type CloseTarget = (String, Arc<dyn BackendSession>, Option<Arc<CloseSlot>>);

/// One unpublished spawn row (the dsh `PendingSpawn`; the abort controller
/// folds into the synchronous `aborted` slot).
struct PendingSpawn {
    token: u64,
    owner: String,
    aborted: Option<TerminalFailure>,
    settled: bool,
    cleanup_failure: Option<String>,
}

/// The guarded registry state.
struct RegistryState {
    backends: Vec<(String, Arc<dyn TerminalBackend>)>,
    sessions: Vec<SessionRecord>,
    reserved_names: HashMap<String, HashSet<String>>,
    pending: Vec<PendingSpawn>,
    disposed_owners: HashSet<String>,
    next_id: u64,
    next_token: u64,
    disposing: bool,
}

impl RegistryState {
    fn assert_active(&self) -> Result<(), TerminalFailure> {
        if self.disposing {
            return Err(TerminalFailure::terminal(
                TerminalErrorCode::ServiceDisposing,
                "PTY service is disposing".to_owned(),
            ));
        }
        Ok(())
    }

    fn expect_owned(
        &mut self,
        owner: &str,
        session_id: &str,
    ) -> Result<&mut SessionRecord, TerminalFailure> {
        let Some(record) = self
            .sessions
            .iter_mut()
            .find(|record| record.id == session_id)
        else {
            return Err(TerminalFailure::terminal(
                TerminalErrorCode::NoSession,
                format!("unknown PTY session {session_id}"),
            ));
        };
        if record.owner != owner {
            return Err(TerminalFailure::terminal(
                TerminalErrorCode::ForeignSession,
                format!("PTY session {session_id} belongs to another agent"),
            ));
        }
        Ok(record)
    }

    fn snapshot(record: &SessionRecord) -> TerminalSessionSnapshot {
        TerminalSessionSnapshot {
            session_id: record.id.clone(),
            name: record.name.clone(),
            backend_type: record.backend_type.clone(),
            pid: record.session.pid(),
            status: record.session.status(),
        }
    }

    fn reserve_spawn(&mut self, owner: &str) -> u64 {
        self.next_token += 1;
        let token = self.next_token;
        self.pending.push(PendingSpawn {
            token,
            owner: owner.to_owned(),
            aborted: None,
            settled: false,
            cleanup_failure: None,
        });
        token
    }

    fn release_spawn(&mut self, token: u64, cleanup_failure: Option<String>) {
        // A cleanup failure stays enlisted until the next dispose sweep
        // collects it (the dsh release shape).
        let retained = cleanup_failure.is_some();
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.token == token)
        {
            pending.cleanup_failure = cleanup_failure;
            pending.settled = true;
        }
        if !retained {
            self.pending.retain(|pending| pending.token != token);
        }
    }

    fn release_name(&mut self, owner: &str, name: Option<&str>) {
        let Some(name) = name else {
            return;
        };
        if let Some(reserved) = self.reserved_names.get_mut(owner) {
            reserved.remove(name);
            if reserved.is_empty() {
                self.reserved_names.remove(owner);
            }
        }
    }
}

/// The in-process registry for replaceable PTY backends and owner-scoped
/// sessions (the dsh `TerminalSessionService`).
pub struct TerminalRegistry {
    /// The injected owner-liveness closure (supplied by the agent service
    /// wiring; the dsh exact-Agent identity check).
    owner_live: Box<dyn Fn(&str) -> bool + Send + Sync>,
    /// The guarded registry state.
    state: Mutex<RegistryState>,
    /// Wakeups for pending-spawn settlement (outside the guarded struct so
    /// the condvar borrow never aliases the guard).
    settled_ready: Condvar,
}

impl fmt::Debug for TerminalRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalRegistry").finish_non_exhaustive()
    }
}

/// The validated spawn reservation carried across the unlocked backend setup.
struct PreparedSpawn {
    backend: Arc<dyn TerminalBackend>,
    token: u64,
    session_id: String,
}

impl TerminalRegistry {
    /// Builds one registry over the injected owner-liveness closure.
    #[must_use]
    pub fn new(owner_live: Box<dyn Fn(&str) -> bool + Send + Sync>) -> Self {
        Self {
            owner_live,
            state: Mutex::new(RegistryState {
                backends: Vec::new(),
                sessions: Vec::new(),
                reserved_names: HashMap::new(),
                pending: Vec::new(),
                disposed_owners: HashSet::new(),
                next_id: 0,
                next_token: 0,
                disposing: false,
            }),
            settled_ready: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, RegistryState> {
        self.state.lock().expect("registry state lock")
    }

    /// Reads the disposing flag (test support for the mid-flight dispose).
    #[cfg(test)]
    pub(crate) fn lock_disposing_for_test(&self) -> bool {
        self.lock().disposing
    }

    /// Counts the enlisted pending spawns (test support).
    #[cfg(test)]
    pub(crate) fn pending_len_for_test(&self) -> usize {
        self.lock().pending.len()
    }

    /// Reads whether one session's closing fence is armed (test support).
    #[cfg(test)]
    pub(crate) fn closing_fence_for_test(&self, owner: &str, session_id: &str) -> bool {
        self.lock()
            .sessions
            .iter()
            .find(|record| record.id == session_id && record.owner == owner)
            .is_some_and(|record| record.closing.is_some())
    }

    fn owner_is_live(&self, state: &RegistryState, owner: &str) -> bool {
        !state.disposed_owners.contains(owner) && (self.owner_live)(owner)
    }

    fn ensure_owner_live(&self, state: &RegistryState, owner: &str) -> Result<(), TerminalFailure> {
        if !self.owner_is_live(state, owner) {
            return Err(TerminalFailure::terminal(
                TerminalErrorCode::OwnerNotLive,
                format!("agent \"{owner}\" is not the registered PTY owner"),
            ));
        }
        Ok(())
    }

    fn reserve_name(
        state: &mut RegistryState,
        owner: &str,
        name: Option<&str>,
    ) -> Result<(), TerminalFailure> {
        let Some(name) = name else {
            return Ok(());
        };
        if state
            .sessions
            .iter()
            .any(|record| record.owner == owner && record.name.as_deref() == Some(name))
        {
            return Err(TerminalFailure::terminal(
                TerminalErrorCode::DuplicateName,
                format!("PTY session name \"{name}\" already exists for this owner"),
            ));
        }
        let reserved = state.reserved_names.entry(owner.to_owned()).or_default();
        if reserved.contains(name) {
            return Err(TerminalFailure::terminal(
                TerminalErrorCode::DuplicateName,
                format!("PTY session name \"{name}\" is already being created"),
            ));
        }
        reserved.insert(name.to_owned());
        Ok(())
    }

    /// Registers one backend type for this effect scope.
    ///
    /// # Errors
    ///
    /// Returns the plain empty-type text and the coded duplicate-backend
    /// text.
    pub fn register_backend(
        &self,
        backend: Arc<dyn TerminalBackend>,
    ) -> Result<(), TerminalFailure> {
        if backend.backend_type().is_empty() {
            return Err(TerminalFailure::plain(
                "pty backend type must be non-empty".to_owned(),
            ));
        }
        let backend_type = backend.backend_type().to_owned();
        {
            let mut state = self.lock();
            if state
                .backends
                .iter()
                .any(|(existing, _)| existing == &backend_type)
            {
                return Err(TerminalFailure::terminal(
                    TerminalErrorCode::DuplicateBackend,
                    format!("a PTY backend named \"{backend_type}\" is already registered"),
                ));
            }
            state.backends.push((backend_type, backend));
        }
        Ok(())
    }

    /// Removes exactly this backend contribution (the dsh disposer).
    pub fn unregister_backend(&self, backend: &Arc<dyn TerminalBackend>) {
        let mut state = self.lock();
        if let Some(position) = state
            .backends
            .iter()
            .position(|(_, registered)| Arc::ptr_eq(registered, backend))
        {
            state.backends.remove(position);
        }
    }

    /// Lists registered backend types in registration order.
    #[must_use]
    pub fn list_backends(&self) -> Vec<String> {
        self.lock()
            .backends
            .iter()
            .map(|(backend_type, _)| backend_type.clone())
            .collect()
    }

    /// Creates and publishes one owner-scoped session after backend setup
    /// succeeds.
    ///
    /// # Errors
    ///
    /// Returns the coded registry failures and the rollback aggregates.
    pub fn spawn(
        &self,
        owner: &str,
        request: &SpawnRequest,
    ) -> Result<TerminalSpawnResult, TerminalFailure> {
        let prepared = self.prepare_spawn(owner, request)?;
        let spawned = prepared.backend.spawn(&BackendSpawnSpec {
            session_id: prepared.session_id.clone(),
            owner: owner.to_owned(),
            backend_type: request.backend_type.clone(),
            name: request.name.clone(),
            cwd: request.cwd.clone(),
        });
        match spawned {
            Ok(session) => self.publish_spawn(owner, request, &prepared, session),
            Err(error) => Err(self.fail_spawn(owner, request, &prepared, &error)),
        }
    }

    /// Validates the request and takes the name and pending-spawn
    /// reservations under the state lock (the dsh pre-setup sequence).
    fn prepare_spawn(
        &self,
        owner: &str,
        request: &SpawnRequest,
    ) -> Result<PreparedSpawn, TerminalFailure> {
        let backend_type = request.backend_type.clone();
        let empty_name = request.name.as_ref().is_some_and(String::is_empty);
        let mut state = self.lock();
        state.assert_active()?;
        self.ensure_owner_live(&state, owner)?;
        let backend = state
            .backends
            .iter()
            .find_map(|(existing, provider)| {
                (existing == &backend_type).then(|| Arc::clone(provider))
            })
            .ok_or_else(|| {
                TerminalFailure::terminal(
                    TerminalErrorCode::NoBackend,
                    format!("no PTY backend registered for \"{backend_type}\""),
                )
            })?;
        if empty_name {
            return Err(TerminalFailure::plain(
                "PTY session name must be non-empty".to_owned(),
            ));
        }
        Self::reserve_name(&mut state, owner, request.name.as_deref())?;
        let token = state.reserve_spawn(owner);
        state.next_id += 1;
        let session_id = format!("pty-{}", state.next_id);
        drop(state);
        Ok(PreparedSpawn {
            backend,
            token,
            session_id,
        })
    }

    /// Re-checks the fences and publishes the session, or rolls the
    /// unpublished session back (the dsh post-setup sequence).
    fn publish_spawn(
        &self,
        owner: &str,
        request: &SpawnRequest,
        prepared: &PreparedSpawn,
        session: Arc<dyn BackendSession>,
    ) -> Result<TerminalSpawnResult, TerminalFailure> {
        let failure = {
            let state = self.lock();
            state
                .pending
                .iter()
                .find(|pending| pending.token == prepared.token)
                .and_then(|pending| pending.aborted.clone())
                .or_else(|| {
                    state.disposing.then(|| {
                        TerminalFailure::terminal(
                            TerminalErrorCode::ServiceDisposing,
                            "PTY service is disposing".to_owned(),
                        )
                    })
                })
                .or_else(|| {
                    (!self.owner_is_live(&state, owner)).then(|| {
                        TerminalFailure::terminal(
                            TerminalErrorCode::OwnerNotLive,
                            "PTY owner is no longer live".to_owned(),
                        )
                    })
                })
        };
        let Some(failure) = failure else {
            let result = TerminalSpawnResult {
                session_id: prepared.session_id.clone(),
                name: request.name.clone(),
                backend_type: request.backend_type.clone(),
                pid: session.pid(),
                status: session.status(),
                motd: session.motd(),
            };
            {
                let mut state = self.lock();
                state.sessions.push(SessionRecord {
                    id: prepared.session_id.clone(),
                    owner: owner.to_owned(),
                    name: request.name.clone(),
                    backend_type: request.backend_type.clone(),
                    session,
                    active: None,
                    closing: None,
                });
                state.release_spawn(prepared.token, None);
                state.release_name(owner, request.name.as_deref());
            }
            self.settled_ready.notify_all();
            return Ok(result);
        };
        let rollback = rollback_session(&session);
        {
            let mut state = self.lock();
            state.release_spawn(prepared.token, rollback.clone());
            state.release_name(owner, request.name.as_deref());
        }
        self.settled_ready.notify_all();
        match rollback {
            Some(close_error) => Err(TerminalFailure::aggregate(
                "PTY spawn and rollback both failed",
                vec![failure.message, close_error],
            )),
            None => Err(failure),
        }
    }

    /// Releases the reservations after a failed backend setup, with the
    /// reservation-abort reason taking failure priority (the dsh catch).
    fn fail_spawn(
        &self,
        owner: &str,
        request: &SpawnRequest,
        prepared: &PreparedSpawn,
        error: &BackendSpawnError,
    ) -> TerminalFailure {
        let failure = {
            let mut state = self.lock();
            let failure = state
                .pending
                .iter()
                .find(|pending| pending.token == prepared.token)
                .and_then(|pending| pending.aborted.clone())
                .unwrap_or_else(|| TerminalFailure::plain(error.message.clone()));
            state.release_spawn(prepared.token, error.cleanup.clone());
            state.release_name(owner, request.name.as_deref());
            failure
        };
        self.settled_ready.notify_all();
        failure
    }

    /// Tests whether an owner holds a published session or an unpublished
    /// spawn (the sandbox-mode fence enforcement criterion).
    #[must_use]
    pub fn has_owner_activity(&self, owner: &str) -> bool {
        let state = self.lock();
        state.pending.iter().any(|pending| pending.owner == owner)
            || state.sessions.iter().any(|record| record.owner == owner)
    }

    /// Starts one exclusive interactive send.
    ///
    /// # Errors
    ///
    /// Returns the ownership, closing-fence, and exclusive-send failures.
    pub fn start_send(
        &self,
        owner: &str,
        session_id: &str,
        text: &str,
        submit: bool,
    ) -> Result<Arc<SendOperation>, TerminalFailure> {
        let operation = {
            let mut state = self.lock();
            let record = state.expect_owned(owner, session_id)?;
            if record.closing.is_some() {
                return Err(TerminalFailure::plain(format!(
                    "PTY session {session_id} is closing"
                )));
            }
            if record
                .active
                .as_ref()
                .is_some_and(|operation| !operation.is_settled())
            {
                return Err(TerminalFailure::terminal(
                    TerminalErrorCode::SendActive,
                    format!("PTY session {session_id} already has an active send"),
                ));
            }
            let operation = Arc::clone(&record.session)
                .start_send(text, submit)
                .map_err(TerminalFailure::plain)?;
            record.active = Some(Arc::clone(&operation));
            drop(state);
            operation
        };
        Ok(operation)
    }

    /// Reads one bounded scrollback page from an owned session.
    ///
    /// # Errors
    ///
    /// Returns the ownership failure and the backend's read validation text.
    pub fn read(
        &self,
        owner: &str,
        session_id: &str,
        offset: Option<usize>,
        count: Option<usize>,
    ) -> Result<ReadPage, TerminalFailure> {
        let session = {
            let mut state = self.lock();
            Arc::clone(&state.expect_owned(owner, session_id)?.session)
        };
        session.read(offset, count).map_err(TerminalFailure::plain)
    }

    /// Delivers an allowed signal through an owned backend session.
    ///
    /// # Errors
    ///
    /// Returns the ownership failure and the backend's delivery failure.
    pub fn signal(
        &self,
        owner: &str,
        session_id: &str,
        name: &str,
    ) -> Result<i64, TerminalFailure> {
        let session = {
            let mut state = self.lock();
            Arc::clone(&state.expect_owned(owner, session_id)?.session)
        };
        session.signal(name).map_err(TerminalFailure::plain)
    }

    /// Closes one owned session behind the closing fence, removing it only
    /// after quiescent backend cleanup.
    ///
    /// # Errors
    ///
    /// Returns the ownership failure and the close failure.
    pub fn kill(
        &self,
        owner: &str,
        session_id: &str,
        reason: &str,
    ) -> Result<bool, TerminalFailure> {
        let slot = {
            let mut state = self.lock();
            let record = state.expect_owned(owner, session_id)?;
            if let Some(existing) = record.closing.clone() {
                drop(state);
                wait_close(&existing).map_err(TerminalFailure::plain)?;
                return Ok(false);
            }
            let slot = Arc::clone(&record.session).close(reason);
            record.closing = Some(Arc::clone(&slot));
            slot
        };
        match wait_close(&slot) {
            Ok(()) => {
                self.lock()
                    .sessions
                    .retain(|record| record.id != session_id);
                Ok(true)
            }
            Err(error) => {
                {
                    let mut state = self.lock();
                    if let Some(record) = state
                        .sessions
                        .iter_mut()
                        .find(|record| record.id == session_id)
                    {
                        // A concurrent retry may already own a newer fence;
                        // never clear it (the dsh closeRecords guard).
                        if record
                            .closing
                            .as_ref()
                            .is_some_and(|closing| Arc::ptr_eq(closing, &slot))
                        {
                            record.closing = None;
                        }
                    }
                }
                Err(TerminalFailure::plain(error))
            }
        }
    }

    /// Lists fresh snapshots for exactly one owner in publication order.
    #[must_use]
    pub fn list(&self, owner: &str) -> Vec<TerminalSessionSnapshot> {
        let state = self.lock();
        state
            .sessions
            .iter()
            .filter(|record| record.owner == owner)
            .map(RegistryState::snapshot)
            .collect()
    }

    /// Marks the owner disposed and closes its sessions and pending spawns
    /// (the dsh owner-cleanup effect body; wired by the agent service
    /// teardown in batch 3).
    ///
    /// # Errors
    ///
    /// Returns the aggregated lifecycle cleanup failure.
    pub fn dispose_owned(&self, owner: &str) -> Result<(), TerminalFailure> {
        self.lock().disposed_owners.insert(owner.to_owned());
        let outcome = self.abort_and_close(
            Some(owner),
            &TerminalFailure::terminal(
                TerminalErrorCode::OwnerNotLive,
                "PTY owner is no longer live".to_owned(),
            ),
            "PTY owner disposed",
        );
        self.lock().reserved_names.remove(owner);
        outcome
    }

    /// Disposes the whole service: aborts unpublished spawns, closes every
    /// session, and clears the backend, reservation, and pending tables.
    ///
    /// # Errors
    ///
    /// Returns the aggregated lifecycle cleanup failure.
    pub fn dispose_all(&self) -> Result<(), TerminalFailure> {
        self.lock().disposing = true;
        let outcome = self.abort_and_close(
            None,
            &TerminalFailure::terminal(
                TerminalErrorCode::ServiceDisposing,
                "PTY service is disposing".to_owned(),
            ),
            "PTY service disposed",
        );
        {
            let mut state = self.lock();
            state.backends.clear();
            state.reserved_names.clear();
            state.pending.clear();
        }
        self.settled_ready.notify_all();
        outcome
    }

    /// Aborts one owner's (or every) pending spawn and closes its sessions,
    /// aggregating both failure families (the dsh `abortAndClose`).
    fn abort_and_close(
        &self,
        owner: Option<&str>,
        abort_reason: &TerminalFailure,
        close_reason: &str,
    ) -> Result<(), TerminalFailure> {
        let mut causes = Vec::new();
        if let Err(error) = self.abort_pending_spawns(owner, abort_reason) {
            causes.push(error.message);
        }
        if let Err(error) = self.close_records(owner, close_reason) {
            causes.push(error.message);
        }
        if causes.is_empty() {
            Ok(())
        } else {
            Err(TerminalFailure::aggregate(
                "failed to clean up PTY lifecycle",
                causes,
            ))
        }
    }

    /// Aborts the pending spawns and waits for their settlement, collecting
    /// the retained cleanup failures (the dsh `abortPendingSpawns`).
    fn abort_pending_spawns(
        &self,
        owner: Option<&str>,
        abort_reason: &TerminalFailure,
    ) -> Result<(), TerminalFailure> {
        let tokens: Vec<u64> = {
            let mut state = self.lock();
            let tokens: Vec<u64> = state
                .pending
                .iter()
                .filter(|pending| owner.is_none_or(|target| pending.owner == target))
                .map(|pending| pending.token)
                .collect();
            for pending in state
                .pending
                .iter_mut()
                .filter(|pending| tokens.contains(&pending.token))
            {
                // controller.abort keeps the first reason (idempotent).
                if pending.aborted.is_none() {
                    pending.aborted = Some(abort_reason.clone());
                }
            }
            drop(state);
            tokens
        };
        {
            let mut state = self.lock();
            while state
                .pending
                .iter()
                .any(|pending| tokens.contains(&pending.token) && !pending.settled)
            {
                state = self.settled_ready.wait(state).expect("registry state lock");
            }
            drop(state);
        }
        let mut failures = Vec::new();
        {
            let mut state = self.lock();
            for token in &tokens {
                if let Some(cleanup_failure) = state
                    .pending
                    .iter()
                    .find(|pending| pending.token == *token)
                    .and_then(|pending| pending.cleanup_failure.clone())
                {
                    failures.push(cleanup_failure);
                }
            }
            state
                .pending
                .retain(|pending| !tokens.contains(&pending.token));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(TerminalFailure::aggregate(
                "failed to roll back unpublished PTY setup",
                failures,
            ))
        }
    }

    /// Closes the matching sessions behind their fences and removes the
    /// settled records, aggregating close failures (the dsh `closeRecords`).
    fn close_records(&self, owner: Option<&str>, reason: &str) -> Result<(), TerminalFailure> {
        let targets: Vec<CloseTarget> = {
            let state = self.lock();
            state
                .sessions
                .iter()
                .filter(|record| owner.is_none_or(|target| record.owner == target))
                .map(|record| {
                    (
                        record.id.clone(),
                        Arc::clone(&record.session),
                        record.closing.clone(),
                    )
                })
                .collect()
        };
        let mut failures = Vec::new();
        for (session_id, session, existing) in targets {
            let slot = existing.unwrap_or_else(|| {
                let slot = session.close(reason);
                {
                    if let Some(record) = self
                        .lock()
                        .sessions
                        .iter_mut()
                        .find(|record| record.id == session_id)
                    {
                        record.closing = Some(Arc::clone(&slot));
                    }
                }
                slot
            });
            match wait_close(&slot) {
                Ok(()) => {
                    self.lock()
                        .sessions
                        .retain(|record| record.id != session_id);
                }
                Err(error) => {
                    {
                        let mut state = self.lock();
                        if let Some(record) = state
                            .sessions
                            .iter_mut()
                            .find(|record| record.id == session_id)
                        {
                            // A concurrent retry may already own a newer
                            // fence; never clear it (the dsh closeRecords
                            // guard).
                            if record
                                .closing
                                .as_ref()
                                .is_some_and(|closing| Arc::ptr_eq(closing, &slot))
                            {
                                record.closing = None;
                            }
                        }
                    }
                    failures.push(error);
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(TerminalFailure::aggregate(
                &format!("failed to close {} PTY session(s)", failures.len()),
                failures,
            ))
        }
    }
}

/// Closes an unpublished session for the spawn rollback, returning the close
/// failure text if any (the dsh `PTY spawn rolled back` cleanup).
fn rollback_session(session: &Arc<dyn BackendSession>) -> Option<String> {
    let slot = Arc::clone(session).close("PTY spawn rolled back");
    wait_close(&slot).err()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::inspect::ProcFs;
    use crate::terminal::{TerminalSpawnSpec, spawn_terminal};
    use crate::terminal_session::{LocalPtySession, SendResult, ShellDialect, WaitReason};

    type Gate = Arc<(Mutex<bool>, Condvar)>;

    fn gate(open: bool) -> Gate {
        Arc::new((Mutex::new(open), Condvar::new()))
    }

    fn open_gate(gate: &Gate) {
        *gate.0.lock().expect("gate lock") = true;
        gate.1.notify_all();
    }

    fn wait_until(mut predicate: impl FnMut() -> bool, label: &str) {
        let deadline = Instant::now() + Duration::from_millis(2000);
        while !predicate() {
            assert!(Instant::now() < deadline, "timed out waiting: {label}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn live_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(true))
    }

    fn registry_with(flag: &Arc<AtomicBool>) -> Arc<TerminalRegistry> {
        let flag = Arc::clone(flag);
        Arc::new(TerminalRegistry::new(Box::new(move |owner| {
            owner.starts_with('a') && flag.load(Ordering::SeqCst)
        })))
    }

    fn canned_send_result() -> SendResult {
        SendResult {
            viewport: String::new(),
            wait_reason: WaitReason::InferredIdle,
            session_status: SessionStatus::Running,
            truncated: false,
        }
    }

    fn fake_session(
        send_op: Option<Arc<SendOperation>>,
        close_gate: Option<Gate>,
        close_error: Option<String>,
    ) -> Arc<FakeSession> {
        Arc::new(FakeSession {
            pid_value: Some(4242),
            status_value: Mutex::new(SessionStatus::Running),
            motd_text: "ready".to_owned(),
            closed_reasons: Mutex::new(Vec::new()),
            close_gate,
            close_error,
            send_op,
            send_error: None,
            read_text: String::new(),
        })
    }

    enum FakeSpawnResult {
        Session(Arc<FakeSession>),
        Fail {
            message: String,
            cleanup: Option<String>,
        },
    }

    struct FakeBackend {
        backend_type: String,
        hold: Option<Gate>,
        live_flag: Option<Arc<AtomicBool>>,
        result: FakeSpawnResult,
    }

    impl TerminalBackend for FakeBackend {
        fn backend_type(&self) -> &str {
            &self.backend_type
        }

        fn spawn(
            &self,
            _spec: &BackendSpawnSpec,
        ) -> Result<Arc<dyn BackendSession>, BackendSpawnError> {
            if let Some(hold) = &self.hold {
                let mut open = hold.0.lock().expect("spawn gate lock");
                while !*open {
                    open = hold.1.wait(open).expect("spawn gate lock");
                }
                drop(open);
            }
            if let Some(live_flag) = &self.live_flag {
                live_flag.store(false, Ordering::SeqCst);
            }
            match &self.result {
                FakeSpawnResult::Session(session) => {
                    Ok(Arc::clone(session) as Arc<dyn BackendSession>)
                }
                FakeSpawnResult::Fail { message, cleanup } => Err(BackendSpawnError {
                    message: message.clone(),
                    cleanup: cleanup.clone(),
                }),
            }
        }
    }

    struct FakeSession {
        pid_value: Option<u32>,
        status_value: Mutex<SessionStatus>,
        motd_text: String,
        closed_reasons: Mutex<Vec<String>>,
        close_gate: Option<Gate>,
        close_error: Option<String>,
        send_op: Option<Arc<SendOperation>>,
        send_error: Option<String>,
        read_text: String,
    }

    impl BackendSession for FakeSession {
        fn pid(&self) -> Option<u32> {
            self.pid_value
        }

        fn status(&self) -> SessionStatus {
            self.status_value
                .lock()
                .expect("fake session status lock")
                .clone()
        }

        fn motd(&self) -> String {
            self.motd_text.clone()
        }

        fn read(&self, offset: Option<usize>, count: Option<usize>) -> Result<ReadPage, String> {
            let total_lines = usize::from(!self.read_text.is_empty());
            Ok(ReadPage {
                text: self.read_text.clone(),
                total_lines,
                line_begin: offset.unwrap_or(0),
                line_end: offset.unwrap_or(0) + count.unwrap_or(1).min(1),
                truncated: false,
            })
        }

        fn signal(&self, _name: &str) -> Result<i64, String> {
            Ok(4242)
        }

        fn start_send(
            self: Arc<Self>,
            _text: &str,
            _submit: bool,
        ) -> Result<Arc<SendOperation>, String> {
            if let Some(error) = &self.send_error {
                return Err(error.clone());
            }
            Ok(self
                .send_op
                .clone()
                .expect("the fake session carries a send operation"))
        }

        fn close(self: Arc<Self>, reason: &str) -> Arc<CloseSlot> {
            self.closed_reasons
                .lock()
                .expect("fake session close lock")
                .push(reason.to_owned());
            let slot = Arc::new(CloseSlot::default());
            let error = self.close_error.clone();
            if let Some(close_gate) = &self.close_gate {
                // The fence returns promptly; a settle thread honors the
                // gate (the BackendSession close contract).
                let gate = Arc::clone(close_gate);
                let settled_slot = Arc::clone(&slot);
                thread::spawn(move || {
                    let mut open = gate.0.lock().expect("close gate lock");
                    while !*open {
                        open = gate.1.wait(open).expect("close gate lock");
                    }
                    drop(open);
                    *settled_slot.result.lock().expect("close lock") =
                        Some(error.map_or(Ok(()), Err));
                    settled_slot.ready.notify_all();
                });
            } else {
                *slot.result.lock().expect("close lock") = Some(error.map_or(Ok(()), Err));
                slot.ready.notify_all();
            }
            slot
        }
    }

    fn request(backend_type: &str, name: Option<&str>) -> SpawnRequest {
        SpawnRequest {
            backend_type: backend_type.to_owned(),
            name: name.map(str::to_owned),
            cwd: None,
        }
    }

    fn registered<B: TerminalBackend + 'static>(
        registry: &TerminalRegistry,
        backend: B,
    ) -> Arc<dyn TerminalBackend> {
        let backend: Arc<dyn TerminalBackend> = Arc::new(backend);
        registry
            .register_backend(Arc::clone(&backend))
            .expect("the test backend registers");
        backend
    }

    fn test_session_config() -> crate::terminal_session::SessionConfig {
        crate::terminal_session::SessionConfig {
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

    /// A real backend that spawns `/bin/cat` in a PTY and completes its
    /// startup sequence (the batch 3 bash backend shape).
    struct RealCatBackend;

    impl TerminalBackend for RealCatBackend {
        fn backend_type(&self) -> &'static str {
            "cat"
        }

        fn spawn(
            &self,
            spec: &BackendSpawnSpec,
        ) -> Result<Arc<dyn BackendSession>, BackendSpawnError> {
            let handle = spawn_terminal(
                &TerminalSpawnSpec {
                    argv: vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "echo ready; exec cat".to_owned(),
                    ],
                    cwd: spec.cwd.clone(),
                    env: vec![
                        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                        ("TERM".to_owned(), "dumb".to_owned()),
                    ],
                    rows: 24,
                    cols: 80,
                    grace_ms: 1000.0,
                },
                Arc::new(ProcFs),
            )
            .map_err(|error| BackendSpawnError {
                message: error,
                cleanup: None,
            })?;
            let session = LocalPtySession::new(handle, test_session_config());
            if let Err(error) = session.initialize() {
                let _ = wait_close(&LocalPtySession::close(&session));
                return Err(BackendSpawnError {
                    message: error,
                    cleanup: None,
                });
            }
            Ok(session as Arc<dyn BackendSession>)
        }
    }

    #[test]
    fn backend_registration_shape() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let empty = Arc::new(EmptyBackend);
        let error = registry.register_backend(empty).unwrap_err();
        assert_eq!(error.message, "pty backend type must be non-empty");
        assert_eq!(error.code, None);

        let first = registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        registered(
            &registry,
            FakeBackend {
                backend_type: "pwsh".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        assert_eq!(registry.list_backends(), vec!["bash", "pwsh"]);

        let duplicate = Arc::new(FakeBackend {
            backend_type: "bash".to_owned(),
            hold: None,
            live_flag: None,
            result: FakeSpawnResult::Session(fake_session(None, None, None)),
        });
        let error = registry.register_backend(duplicate).unwrap_err();
        assert_eq!(
            error.message,
            "a PTY backend named \"bash\" is already registered"
        );
        assert_eq!(error.code, Some(TerminalErrorCode::DuplicateBackend));

        registry.unregister_backend(&first);
        assert_eq!(registry.list_backends(), vec!["pwsh"]);
    }

    struct EmptyBackend;

    impl TerminalBackend for EmptyBackend {
        fn backend_type(&self) -> &'static str {
            ""
        }

        fn spawn(
            &self,
            _spec: &BackendSpawnSpec,
        ) -> Result<Arc<dyn BackendSession>, BackendSpawnError> {
            unreachable!("the empty-type backend never spawns")
        }
    }

    #[test]
    fn spawn_validates_backend_and_name() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let missing = registry.spawn("a1", &request("bash", None)).unwrap_err();
        assert_eq!(missing.message, "no PTY backend registered for \"bash\"");
        assert_eq!(missing.code, Some(TerminalErrorCode::NoBackend));

        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        let empty_name = registry
            .spawn("a1", &request("bash", Some("")))
            .unwrap_err();
        assert_eq!(empty_name.message, "PTY session name must be non-empty");
        assert_eq!(empty_name.code, None);
    }

    #[test]
    fn spawn_rejects_duplicate_names() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        registry
            .spawn("a1", &request("bash", Some("main")))
            .expect("the first named spawn publishes");
        let published = registry
            .spawn("a1", &request("bash", Some("main")))
            .unwrap_err();
        assert_eq!(
            published.message,
            "PTY session name \"main\" already exists for this owner"
        );
        assert_eq!(published.code, Some(TerminalErrorCode::DuplicateName));

        let held = gate(false);
        registered(
            &registry,
            FakeBackend {
                backend_type: "pwsh".to_owned(),
                hold: Some(Arc::clone(&held)),
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        let spawner = Arc::clone(&registry);
        let in_flight = thread::spawn(move || {
            spawner.spawn(
                "a1",
                &SpawnRequest {
                    backend_type: "pwsh".to_owned(),
                    name: Some("held".to_owned()),
                    cwd: None,
                },
            )
        });
        wait_until(
            || registry.pending_len_for_test() == 1,
            "the held spawn to enlist its pending fence",
        );
        // The in-flight spawn already reserved the name, so the duplicate
        // surfaces the reserved-name text from the second call.
        let reserved = registry
            .spawn("a1", &request("bash", Some("held")))
            .unwrap_err();
        assert_eq!(
            reserved.message,
            "PTY session name \"held\" is already being created"
        );
        assert_eq!(reserved.code, Some(TerminalErrorCode::DuplicateName));

        open_gate(&held);
        let spawn = in_flight
            .join()
            .expect("the held spawn thread joins")
            .expect("the released spawn publishes");
        assert_eq!(spawn.session_id, "pty-2");
    }

    #[test]
    fn spawn_rejects_dead_owner_at_entry() {
        let flag = live_flag();
        flag.store(false, Ordering::SeqCst);
        let registry = registry_with(&flag);
        let error = registry.spawn("a1", &request("bash", None)).unwrap_err();
        assert_eq!(
            error.message,
            "agent \"a1\" is not the registered PTY owner"
        );
        assert_eq!(error.code, Some(TerminalErrorCode::OwnerNotLive));
        assert!(!registry.has_owner_activity("a1"));
    }

    #[test]
    fn spawn_owner_death_rolls_back() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let session = fake_session(None, None, None);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: Some(Arc::clone(&flag)),
                result: FakeSpawnResult::Session(Arc::clone(&session)),
            },
        );
        let error = registry.spawn("a1", &request("bash", None)).unwrap_err();
        assert_eq!(error.message, "PTY owner is no longer live");
        assert_eq!(error.code, Some(TerminalErrorCode::OwnerNotLive));
        assert_eq!(
            session
                .closed_reasons
                .lock()
                .expect("close lock")
                .as_slice(),
            ["PTY spawn rolled back"]
        );
        assert_eq!(registry.list("a1"), Vec::new());
        assert!(!registry.has_owner_activity("a1"));
    }

    #[test]
    fn spawn_service_disposing_mid_flight() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let held = gate(false);
        let session = fake_session(None, None, None);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: Some(Arc::clone(&held)),
                live_flag: None,
                result: FakeSpawnResult::Session(Arc::clone(&session)),
            },
        );
        let spawner = Arc::clone(&registry);
        let in_flight = thread::spawn(move || spawner.spawn("a1", &request("bash", None)));
        wait_until(
            || registry.has_owner_activity("a1"),
            "the held spawn to enlist its pending fence",
        );
        let disposer = Arc::clone(&registry);
        let disposing = thread::spawn(move || disposer.dispose_all());
        wait_until(
            || registry.lock_disposing_for_test(),
            "the dispose sweep to mark the service disposing",
        );
        open_gate(&held);
        let spawn = in_flight.join().expect("the spawn thread joins");
        let error = spawn.unwrap_err();
        assert_eq!(error.message, "PTY service is disposing");
        assert_eq!(error.code, Some(TerminalErrorCode::ServiceDisposing));
        assert_eq!(
            session
                .closed_reasons
                .lock()
                .expect("close lock")
                .as_slice(),
            ["PTY spawn rolled back"]
        );
        disposing
            .join()
            .expect("the dispose thread joins")
            .expect("the dispose sweep succeeds");

        let after = registry.spawn("a1", &request("bash", None)).unwrap_err();
        assert_eq!(after.message, "PTY service is disposing");
        assert_eq!(after.code, Some(TerminalErrorCode::ServiceDisposing));
        assert_eq!(registry.list_backends(), Vec::<String>::new());
    }

    #[test]
    fn owned_lookup_errors() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        let missing = registry.read("a1", "pty-9", None, None).unwrap_err();
        assert_eq!(missing.message, "unknown PTY session pty-9");
        assert_eq!(missing.code, Some(TerminalErrorCode::NoSession));

        let spawned = registry
            .spawn("a1", &request("bash", None))
            .expect("the owned spawn publishes");
        let foreign = registry
            .read("a2", &spawned.session_id, None, None)
            .unwrap_err();
        assert_eq!(
            foreign.message,
            format!(
                "PTY session {} belongs to another agent",
                spawned.session_id
            )
        );
        assert_eq!(foreign.code, Some(TerminalErrorCode::ForeignSession));
    }

    #[test]
    fn start_send_exclusive_and_settled_release() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let operation = SendOperation::new(4 * 1024, Instant::now());
        let close_gate = gate(false);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(
                    Some(Arc::clone(&operation)),
                    Some(Arc::clone(&close_gate)),
                    None,
                )),
            },
        );
        let spawned = registry
            .spawn("a1", &request("bash", None))
            .expect("the owned spawn publishes");
        let first = registry
            .start_send("a1", &spawned.session_id, "echo hi", true)
            .expect("the first send starts");
        let second = registry
            .start_send("a1", &spawned.session_id, "echo again", true)
            .unwrap_err();
        assert_eq!(
            second.message,
            format!(
                "PTY session {} already has an active send",
                spawned.session_id
            )
        );
        assert_eq!(second.code, Some(TerminalErrorCode::SendActive));

        first.test_settle(canned_send_result());
        wait_until(|| first.is_settled(), "the canned settle to land");
        registry
            .start_send("a1", &spawned.session_id, "echo third", true)
            .expect("the settled send releases the exclusive slot");

        // During the gated close the send faces the closing fence.
        let closer = Arc::clone(&registry);
        let kill_id = spawned.session_id.clone();
        let closer_thread = thread::spawn(move || closer.kill("a1", &kill_id, "test done"));
        let mut closing = None;
        wait_until(
            || {
                closing = registry
                    .start_send("a1", &spawned.session_id, "echo late", true)
                    .err();
                closing
                    .as_ref()
                    .is_some_and(|failure| failure.message.ends_with("is closing"))
            },
            "the closing fence to face the send",
        );
        assert_eq!(closing.expect("the closing failure reads").code, None);
        open_gate(&close_gate);
        assert!(
            closer_thread
                .join()
                .expect("the fenced close joins")
                .expect("the close settles")
        );
        // After the quiescent close the session is removed (dsh semantics).
        let gone = registry
            .start_send("a1", &spawned.session_id, "echo late", true)
            .unwrap_err();
        assert_eq!(
            gone.message,
            format!("unknown PTY session {}", spawned.session_id)
        );
    }

    #[test]
    fn kill_fence_shares_one_close_under_concurrency() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let settle_gate = gate(false);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(
                    None,
                    Some(Arc::clone(&settle_gate)),
                    Some("boom".to_owned()),
                )),
            },
        );
        let spawned = registry
            .spawn("a1", &request("bash", None))
            .expect("the owned spawn publishes");
        let closer = Arc::clone(&registry);
        let session_id = spawned.session_id.clone();
        let first = thread::spawn(move || closer.kill("a1", &session_id, "done"));
        let fence_id = spawned.session_id.clone();
        wait_until(
            || registry.closing_fence_for_test("a1", &fence_id),
            "the closing fence to arm",
        );
        let second_caller = Arc::clone(&registry);
        let second_id = spawned.session_id.clone();
        let second = thread::spawn(move || second_caller.kill("a1", &second_id, "again"));
        open_gate(&settle_gate);
        let first_error = first
            .join()
            .expect("the first kill joins")
            .expect_err("the failing close reports");
        assert_eq!(first_error.message, "boom");
        // The concurrent killer shares one fence and one failure under every
        // interleaving: it either awaits the same fence or retries the close
        // after the fence cleared.
        let second_error = second
            .join()
            .expect("the second kill joins")
            .expect_err("the shared close failure reports");
        assert_eq!(second_error.message, "boom");
        // A failed close keeps its record enlisted with a cleared fence.
        let listed = registry.list("a1");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, spawned.session_id);
        assert!(!registry.closing_fence_for_test("a1", &fence_id));
    }

    #[test]
    fn has_owner_activity_spans_spawn_to_close() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        assert!(!registry.has_owner_activity("a1"));
        let held = gate(false);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: Some(Arc::clone(&held)),
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        let holder = Arc::clone(&registry);
        let in_flight = thread::spawn(move || holder.spawn("a1", &request("bash", None)));
        wait_until(
            || registry.has_owner_activity("a1"),
            "the unpublished spawn to count as activity",
        );
        open_gate(&held);
        let spawned = in_flight
            .join()
            .expect("the held spawn joins")
            .expect("the held spawn publishes");
        assert!(registry.has_owner_activity("a1"));
        registry
            .kill("a1", &spawned.session_id, "done")
            .expect("the close settles");
        assert!(!registry.has_owner_activity("a1"));
    }

    #[test]
    fn dispose_owned_closes_and_marks_owner_gone() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        let session = fake_session(None, None, None);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(Arc::clone(&session)),
            },
        );
        let spawned = registry
            .spawn("a1", &request("bash", Some("main")))
            .expect("the named spawn publishes");
        registry
            .dispose_owned("a1")
            .expect("the owner dispose succeeds");
        assert_eq!(
            session
                .closed_reasons
                .lock()
                .expect("close lock")
                .as_slice(),
            ["PTY owner disposed"]
        );
        assert_eq!(registry.list("a1"), Vec::new());
        assert_ne!(spawned.session_id, String::new());

        let gone = registry.spawn("a1", &request("bash", None)).unwrap_err();
        assert_eq!(gone.message, "agent \"a1\" is not the registered PTY owner");
        // The freed name reservation no longer blocks another owner.
        registered(
            &registry,
            FakeBackend {
                backend_type: "pwsh".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        registry
            .spawn("a2", &request("pwsh", Some("main")))
            .expect("the freed name reservation releases");
    }

    #[test]
    fn dispose_all_aggregates_close_failures() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(
                    None,
                    None,
                    Some("close boom".to_owned()),
                )),
            },
        );
        registered(
            &registry,
            FakeBackend {
                backend_type: "pwsh".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Session(fake_session(None, None, None)),
            },
        );
        registry
            .spawn("a1", &request("bash", None))
            .expect("the failing-close spawn publishes");
        registry
            .spawn("a2", &request("pwsh", None))
            .expect("the clean spawn publishes");
        let error = registry.dispose_all().unwrap_err();
        assert_eq!(error.message, "failed to clean up PTY lifecycle");
        assert!(
            error
                .causes
                .contains(&"failed to close 1 PTY session(s)".to_owned())
        );
        // A failed close leaves its record enlisted (the dsh shape).
        assert_eq!(registry.list("a1").len(), 1);
        assert_eq!(registry.list("a2"), Vec::new());

        let after = registry.spawn("a2", &request("pwsh", None)).unwrap_err();
        assert_eq!(after.message, "PTY service is disposing");
    }

    #[test]
    fn backend_cleanup_failure_rolls_into_dispose() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        registered(
            &registry,
            FakeBackend {
                backend_type: "bash".to_owned(),
                hold: None,
                live_flag: None,
                result: FakeSpawnResult::Fail {
                    message: "boom".to_owned(),
                    cleanup: Some("cleanup boom".to_owned()),
                },
            },
        );
        let error = registry.spawn("a1", &request("bash", None)).unwrap_err();
        assert_eq!(error.message, "boom");
        assert_eq!(error.code, None);
        // The retained cleanup failure keeps the owner visibly active.
        assert!(registry.has_owner_activity("a1"));

        let dispose_error = registry.dispose_all().unwrap_err();
        assert_eq!(dispose_error.message, "failed to clean up PTY lifecycle");
        assert!(
            dispose_error
                .causes
                .contains(&"failed to roll back unpublished PTY setup".to_owned())
        );
        assert!(!registry.has_owner_activity("a1"));
    }

    #[test]
    fn registry_publishes_real_local_session() {
        let flag = live_flag();
        let registry = registry_with(&flag);
        registered(&registry, RealCatBackend);
        let spawned = registry
            .spawn("a1", &request("cat", None))
            .expect("the real local session publishes");
        assert_eq!(spawned.session_id, "pty-1");
        assert_eq!(spawned.backend_type, "cat");
        assert!(spawned.pid.is_some());
        assert_eq!(spawned.status, SessionStatus::Running);
        assert!(spawned.motd.contains("ready"));

        let listed = registry.list("a1");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].session_id, spawned.session_id);
        assert!(registry.has_owner_activity("a1"));

        assert!(
            registry
                .kill("a1", &spawned.session_id, "test done")
                .expect("the real close settles")
        );
        assert_eq!(registry.list("a1"), Vec::new());
        assert!(!registry.has_owner_activity("a1"));
    }
}
