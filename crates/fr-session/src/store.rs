//! The publication store over live sessions (dsh core/session index.ts
//! store section :1018-1345).
//!
//! Owns the prepare/enter/announce/detach lifecycle transactions, the four
//! listener feeds with per-listener containment, the flush checkpoint, and
//! fork/restore entry.
//!
//! Divergences by construction (none behavioral where reachable):
//! - listener callbacks are synchronous; the dsh async-rejection warn branch
//!   (`listener rejected: …`) has no counterpart and is absent;
//! - dsh's `attachments` object-identity map has no Rust counterpart
//!   (sessions move into the store); the double-attach throw is
//!   structurally unreachable and omitted;
//! - append reentry is impossible under `&mut self`; the dsh reentry throw
//!   is kept as a defensive check with its verbatim text;
//! - dsh fork's non-integer/negative boundary arm is unreachable over the
//!   typed `u64` boundary; its text is not carried.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use fr_wire::event::{Origin, SessionEvent, SessionEventData, SessionHeader};
use fr_wire::ids::SessionId;

use crate::error::SessionError;
use crate::session::{Session, SurfaceIntent};

/// Errors from one listener invocation (the listener-authored message,
/// carried verbatim through containment).
pub type ListenerError = String;

type CreatedListener = Arc<dyn Fn(&SessionId) -> Result<(), ListenerError> + Send + Sync>;
type EventListener =
    Arc<dyn Fn(&SessionId, &SessionEvent) -> Result<(), ListenerError> + Send + Sync>;
type SimpleListener = Arc<dyn Fn(&SessionId) -> Result<(), ListenerError> + Send + Sync>;

/// One-shot detach capability returned by [`SessionStore::enter`].
#[derive(Debug)]
pub struct DetachHandle {
    id: SessionId,
    generation: u64,
    used: bool,
}

/// Identity of one store listener across the four registries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriberId(u64);

/// Creation metadata (dsh `PrepareSessionOptions.meta`).
#[derive(Debug, Clone, Default)]
pub struct SessionMeta {
    /// Header `createdAt`; absent = the store clock at prepare time.
    pub created_at: Option<i64>,
    /// Header `cwd`; must be absolute when present (storage keys
    /// directories off it).
    pub cwd: Option<String>,
    /// Fork lineage, set by [`SessionStore::fork`].
    pub parent_session: Option<SessionId>,
    /// Whether the seed carries a fork-inherited prefix.
    pub is_seeded: bool,
    /// Coarse product classification for a subagent child.
    pub origin: Option<Origin>,
    /// Subagent recursion depth; absent (zero) at top level.
    pub delegation_depth: Option<u64>,
    /// Agent preset the session's agent was composed from.
    pub agent_preset: Option<String>,
}

/// Publication state of one entry: idle before `announce`, announcing while
/// the created dispatch is open, announced from the mark-before-emit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnnounceState {
    Idle,
    Announcing,
    Announced,
}

/// All mutable lifecycle state for one exact store entry.
#[derive(Debug)]
struct SessionEntry {
    session: Session,
    generation: u64,
    state: AnnounceState,
    appending: bool,
    detach_requested: bool,
}

/// The publication store over live sessions (dsh `SessionStore`).
pub struct SessionStore {
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    warn: Arc<dyn Fn(String) + Send + Sync>,
    counter: u64,
    lifecycle: u64,
    subscriber_seq: u64,
    entries: HashMap<SessionId, SessionEntry>,
    created: Vec<(SubscriberId, CreatedListener)>,
    event: Vec<(SubscriberId, EventListener)>,
    flush: Vec<(SubscriberId, SimpleListener)>,
    disposed: Vec<(SubscriberId, SimpleListener)>,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionStore")
            .field("counter", &self.counter)
            .field("lifecycle", &self.lifecycle)
            .field("subscriber_seq", &self.subscriber_seq)
            .field("entries", &self.entries.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SessionStore {
    /// Creates a store with the injected clock (header `createdAt`) and
    /// warn sink (listener containment diagnostics).
    pub fn new(
        clock: Arc<dyn Fn() -> i64 + Send + Sync>,
        warn: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        Self {
            clock,
            warn,
            counter: 0,
            lifecycle: 0,
            subscriber_seq: 0,
            entries: HashMap::new(),
            created: Vec::new(),
            event: Vec::new(),
            flush: Vec::new(),
            disposed: Vec::new(),
        }
    }

    /// Registers a `session/created` listener. A listener error vetoes the
    /// publication (dsh synchronous throw semantics).
    pub fn subscribe_created(&mut self, listener: CreatedListener) -> SubscriberId {
        self.subscriber_seq += 1;
        let id = SubscriberId(self.subscriber_seq);
        self.created.push((id, listener));
        id
    }

    /// Registers a `session/event` feed listener (per-listener containment).
    pub fn subscribe_event(&mut self, listener: EventListener) -> SubscriberId {
        self.subscriber_seq += 1;
        let id = SubscriberId(self.subscriber_seq);
        self.event.push((id, listener));
        id
    }

    /// Registers a `session/flush` durability listener. Registration order
    /// decides which failure `flush` reports.
    pub fn subscribe_flush(&mut self, listener: SimpleListener) -> SubscriberId {
        self.subscriber_seq += 1;
        let id = SubscriberId(self.subscriber_seq);
        self.flush.push((id, listener));
        id
    }

    /// Registers a `session/disposed` listener (per-listener containment).
    pub fn subscribe_disposed(&mut self, listener: SimpleListener) -> SubscriberId {
        self.subscriber_seq += 1;
        let id = SubscriberId(self.subscriber_seq);
        self.disposed.push((id, listener));
        id
    }

    /// Removes one listener from every registry; true when it was found.
    pub fn unsubscribe(&mut self, id: SubscriberId) -> bool {
        let mut removed = false;
        let before = self.created.len();
        self.created.retain(|(sid, _)| *sid != id);
        removed |= self.created.len() != before;
        let before = self.event.len();
        self.event.retain(|(sid, _)| *sid != id);
        removed |= self.event.len() != before;
        let before = self.flush.len();
        self.flush.retain(|(sid, _)| *sid != id);
        removed |= self.flush.len() != before;
        let before = self.disposed.len();
        self.disposed.retain(|(sid, _)| *sid != id);
        removed |= self.disposed.len() != before;
        removed
    }

    /// Mints `session-<n>` skipping live ids (dsh counter retry).
    fn mint_id(&mut self) -> SessionId {
        loop {
            self.counter += 1;
            let id = SessionId::new(format!("session-{}", self.counter));
            if !self.entries.contains_key(&id) {
                return id;
            }
        }
    }

    /// Builds a session without entering it: validates the id and metadata,
    /// constructs the header, and replays any seed (dsh `prepare`).
    ///
    /// `inherited_event_count` is the fork-inherited prefix length (zero for
    /// fresh or fully-restored sessions).
    ///
    /// # Errors
    ///
    /// `session "X" already exists` on a live duplicate; a non-absolute
    /// `meta.cwd`; a seed that fails contiguity/admission/surface.
    pub fn prepare(
        &mut self,
        id: Option<SessionId>,
        meta: &SessionMeta,
        seed: Vec<SessionEvent>,
        inherited_event_count: u64,
    ) -> Result<Session, SessionError> {
        let session_id = match id {
            Some(id) => {
                if self.entries.contains_key(&id) {
                    return Err(SessionError::Other(format!(
                        "session \"{id}\" already exists"
                    )));
                }
                id
            }
            None => self.mint_id(),
        };
        if let Some(cwd) = &meta.cwd {
            // dsh prepare @throws: "meta.cwd is a non-absolute path
            // (storage backends key directories off it)".
            if !std::path::Path::new(cwd).is_absolute() {
                return Err(SessionError::Other(
                    "meta.cwd must be an absolute path".to_owned(),
                ));
            }
        }
        let created_at = meta.created_at.unwrap_or_else(|| (self.clock)());
        let header = SessionHeader {
            version: 4,
            id: session_id,
            created_at,
            cwd: meta.cwd.clone(),
            parent_session: meta.parent_session.clone(),
            is_seeded: meta.is_seeded,
            origin: meta.origin,
            delegation_depth: meta.delegation_depth,
            agent_preset: meta.agent_preset.clone(),
        };
        Session::from_seed_parts(header, seed, inherited_event_count)
    }

    /// Enters a prepared session: the authoritative duplicate collision
    /// boundary plus publication-hook state; returns the one-shot detach
    /// capability (dsh `enter`, index.ts:1210-1244).
    ///
    /// # Errors
    ///
    /// `session "X" already exists` when a live entry holds the id.
    pub fn enter(&mut self, session: Session) -> Result<DetachHandle, SessionError> {
        let id = session.header().id.clone();
        if self.entries.contains_key(&id) {
            return Err(SessionError::Other(format!(
                "session \"{id}\" already exists"
            )));
        }
        self.lifecycle += 1;
        let generation = self.lifecycle;
        self.entries.insert(
            id.clone(),
            SessionEntry {
                session,
                generation,
                state: AnnounceState::Idle,
                appending: false,
                detach_requested: false,
            },
        );
        Ok(DetachHandle {
            id,
            generation,
            used: false,
        })
    }

    /// One transaction: prepare + enter + announce; a vetoing
    /// `session/created` listener rolls the attach back with its paired
    /// disposal edge (dsh `create`, index.ts:1111-1122). Resume is the same
    /// transaction over an explicit id and a stored-log seed.
    ///
    /// # Errors
    ///
    /// `prepare`/`enter` duplicates; a vetoing created listener.
    pub fn create(
        &mut self,
        id: Option<SessionId>,
        meta: &SessionMeta,
        seed: Vec<SessionEvent>,
        inherited_event_count: u64,
    ) -> Result<SessionId, SessionError> {
        let session = self.prepare(id, meta, seed, inherited_event_count)?;
        let mut detach = self.enter(session)?;
        let session_id = detach.id.clone();
        if let Err(err) = self.announce(&session_id) {
            self.detach(&mut detach);
            return Err(SessionError::Other(err.to_string()));
        }
        Ok(session_id)
    }

    /// Emits `session/created` exactly once: the mark-before-emit guard, the
    /// announcing window, sequential vetoing listeners, and the deferred
    /// detach (dsh `announce`, index.ts:1265-1293).
    ///
    /// # Errors
    ///
    /// `session "X" is not live in this store`; `session "X" was already
    /// announced`; a listener-authored veto error.
    pub fn announce(&mut self, id: &SessionId) -> Result<(), SessionError> {
        {
            let entry = self.live_entry_mut(id)?;
            if entry.state != AnnounceState::Idle {
                return Err(SessionError::Other(format!(
                    "session \"{id}\" was already announced"
                )));
            }
            entry.state = AnnounceState::Announcing;
        }
        let listeners: Vec<CreatedListener> =
            self.created.iter().map(|(_, l)| Arc::clone(l)).collect();
        let mut veto: Option<String> = None;
        for listener in listeners {
            if let Err(err) = listener(id) {
                veto = Some(err);
                break;
            }
        }
        self.finish_announce(id, veto)
    }

    /// The announce unwind: clear the announcing window, honor a
    /// detach requested during it, then surface the veto.
    fn finish_announce(
        &mut self,
        id: &SessionId,
        veto: Option<String>,
    ) -> Result<(), SessionError> {
        let detach_now = {
            let Some(entry) = self.entries.get_mut(id) else {
                return Err(SessionError::Other(format!(
                    "session \"{id}\" is not live in this store"
                )));
            };
            if entry.state == AnnounceState::Announcing {
                entry.state = AnnounceState::Announced;
            }
            let requested = entry.detach_requested && !entry.appending;
            if requested {
                entry.detach_requested = false;
            }
            requested
        };
        if detach_now {
            self.detach_entered(id);
        }
        veto.map_or(Ok(()), |err| Err(SessionError::Other(err)))
    }

    /// Consumes one detach capability: single-shot, generation-checked
    /// (a stale capability cannot remove a later same-id lifecycle), and
    /// deferred while an announce or append publication is open
    /// (dsh `enter` detach + `detachEntered`, index.ts:1230-1256).
    pub fn detach(&mut self, handle: &mut DetachHandle) {
        if handle.used {
            return;
        }
        handle.used = true;
        let matches = self
            .entries
            .get(&handle.id)
            .is_some_and(|entry| entry.generation == handle.generation);
        if !matches {
            return;
        }
        let deferred = {
            let Some(entry) = self.entries.get_mut(&handle.id) else {
                return;
            };
            if entry.state == AnnounceState::Announcing || entry.appending {
                entry.detach_requested = true;
                true
            } else {
                false
            }
        };
        if !deferred {
            self.detach_entered(&handle.id);
        }
    }

    /// Removes one exact entered session and emits its paired disposal when
    /// announced (dsh `detachEntered`, index.ts:1247-1256).
    fn detach_entered(&mut self, id: &SessionId) {
        let Some(entry) = self.entries.remove(id) else {
            return;
        };
        if entry.state != AnnounceState::Idle {
            let listeners: Vec<SimpleListener> =
                self.disposed.iter().map(|(_, l)| Arc::clone(l)).collect();
            for listener in listeners {
                if let Err(err) = listener(id) {
                    (self.warn)(format!(
                        "session \"{id}\": session/disposed listener threw: {err}"
                    ));
                }
            }
        }
    }

    /// The exact live entry, or the dsh not-live text.
    fn live_entry_mut(&mut self, id: &SessionId) -> Result<&mut SessionEntry, SessionError> {
        self.entries.get_mut(id).ok_or_else(|| {
            SessionError::Other(format!("session \"{id}\" is not live in this store"))
        })
    }

    /// Looks up one live session.
    #[must_use]
    pub fn get(&self, id: &SessionId) -> Option<&Session> {
        self.entries.get(id).map(|entry| &entry.session)
    }

    /// Mutably looks up one live session.
    pub fn get_mut(&mut self, id: &SessionId) -> Option<&mut Session> {
        self.entries.get_mut(id).map(|entry| &mut entry.session)
    }

    /// Appends one surface-eligible event through the store: listener
    /// snapshot before the commit, per-listener contained feed after it,
    /// deferred detach honored at unwind (dsh `Session.commit`,
    /// index.ts:876-897).
    ///
    /// # Errors
    ///
    /// `live_entry_mut` not-live; the defensive reentry throw; the owning
    /// session append check.
    pub fn append_surface(
        &mut self,
        id: &SessionId,
        time: i64,
        data: SessionEventData,
        intent: SurfaceIntent,
    ) -> Result<(), SessionError> {
        let listeners: Vec<EventListener> = self.event.iter().map(|(_, l)| Arc::clone(l)).collect();
        self.append_entry(id, |session| session.append_surface(time, data, intent))?;
        self.feed_event(id, &listeners);
        self.finish_append(id);
        Ok(())
    }

    /// Appends one log-only event through the store (same feed contract as
    /// [`Self::append_surface`]).
    ///
    /// # Errors
    ///
    /// Same contract as [`Self::append_surface`].
    pub fn append_log_only(
        &mut self,
        id: &SessionId,
        time: i64,
        data: SessionEventData,
    ) -> Result<(), SessionError> {
        let listeners: Vec<EventListener> = self.event.iter().map(|(_, l)| Arc::clone(l)).collect();
        self.append_entry(id, |session| session.append_log_only(time, data))?;
        self.feed_event(id, &listeners);
        self.finish_append(id);
        Ok(())
    }

    /// Commits one experimental plugin record through the store.
    ///
    /// # Errors
    ///
    /// Same contract as [`Self::append_surface`].
    pub fn append_record(
        &mut self,
        id: &SessionId,
        time: i64,
        type_name: &str,
        data: serde_json::Value,
    ) -> Result<(), SessionError> {
        let listeners: Vec<EventListener> = self.event.iter().map(|(_, l)| Arc::clone(l)).collect();
        self.append_entry(id, |session| session.append_record(time, type_name, data))?;
        self.feed_event(id, &listeners);
        self.finish_append(id);
        Ok(())
    }

    /// The shared append body: not-live check, the defensive reentry throw
    /// (verbatim dsh text), the appending window, the owning session check.
    fn append_entry(
        &mut self,
        id: &SessionId,
        append: impl FnOnce(&mut Session) -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let entry = self.live_entry_mut(id)?;
        if entry.appending {
            return Err(SessionError::Other(
                "session append cannot reenter while another append is being published".to_owned(),
            ));
        }
        entry.appending = true;
        let result = append(&mut entry.session);
        entry.appending = false;
        result
    }

    /// Feeds one committed event to the pre-snapshot listener list with
    /// per-listener containment (dsh `invokeContainedSessionObservers`,
    /// index.ts:398-416).
    fn feed_event(&self, id: &SessionId, listeners: &[EventListener]) {
        let Some(event) = self
            .entries
            .get(id)
            .and_then(|entry| entry.session.events().last())
        else {
            return;
        };
        for listener in listeners {
            if let Err(err) = listener(id, event) {
                (self.warn)(format!(
                    "session \"{id}\": session/event listener threw: {err}"
                ));
            }
        }
    }

    /// Honors a detach requested during an append publication
    /// (dsh commit finally, index.ts:891-895).
    fn finish_append(&mut self, id: &SessionId) {
        let detach_now = self.entries.get_mut(id).is_some_and(|entry| {
            if entry.detach_requested && !entry.appending {
                entry.detach_requested = false;
                true
            } else {
                false
            }
        });
        if detach_now {
            self.detach_entered(id);
        }
    }

    /// THE flush entry point: snapshot, settle every listener, then report
    /// the FIRST registered failure; returns whether any durability listener
    /// participated (dsh `SessionStore.flush`, index.ts:1319-1336).
    ///
    /// # Errors
    ///
    /// `live_entry_mut` not-live; the first registered listener failure
    /// after every listener has settled.
    pub fn flush(&mut self, id: &SessionId) -> Result<bool, SessionError> {
        let listeners: Vec<SimpleListener> =
            self.flush.iter().map(|(_, l)| Arc::clone(l)).collect();
        self.live_entry_mut(id)?;
        let mut first_failure: Option<String> = None;
        for listener in &listeners {
            if let Err(err) = listener(id)
                && first_failure.is_none()
            {
                first_failure = Some(err);
            }
        }
        first_failure.map_or(Ok(!listeners.is_empty()), |err| {
            Err(SessionError::Other(err))
        })
    }

    /// Forks one live session into a child over an exact event prefix
    /// (dsh `store.fork` rejection family + `buildForkSeed`).
    ///
    /// The child inherits the source `cwd`, carries `parentSession`, and is
    /// seeded `isSeeded: true` with `inheritedEventCount = boundary + 1`
    /// (`0` for the default boundary over an empty source: no seed).
    ///
    /// # Errors
    ///
    /// `session "X" already exists` for a live child id; `session "X" is
    /// not live in this store` for an unknown source; the boundary family
    /// below; a vetoing created listener on the child.
    pub fn fork(
        &mut self,
        source: &SessionId,
        child_id: SessionId,
        boundary: Option<u64>,
    ) -> Result<SessionId, SessionError> {
        if self.entries.contains_key(&child_id) {
            return Err(SessionError::Other(format!(
                "session \"{child_id}\" already exists"
            )));
        }
        let empty_source = self
            .entries
            .get(source)
            .is_some_and(|entry| entry.session.seq() == 0);
        if boundary.is_none() && empty_source {
            // Default boundary over an empty source: no seed.
            let cwd = self
                .entries
                .get(source)
                .and_then(|entry| entry.session.header().cwd.clone());
            return self.create(
                Some(child_id),
                &SessionMeta {
                    cwd,
                    parent_session: Some(source.clone()),
                    is_seeded: true,
                    ..SessionMeta::default()
                },
                Vec::new(),
                0,
            );
        }
        let (cwd, seed, inherited) = {
            let entry = self.live_entry_mut(source)?;
            let cwd = entry.session.header().cwd.clone();
            let last = entry.session.seq();
            let boundary = boundary.unwrap_or_else(|| last - 1);
            if boundary >= last {
                let tail = if last == 0 {
                    "none".to_owned()
                } else {
                    format!("{}", last - 1)
                };
                return Err(SessionError::Other(format!(
                    "fork boundary {boundary} does not exist in session \"{source}\" (last seq: {tail})"
                )));
            }
            if entry
                .session
                .event_at(boundary)
                .is_none_or(|event| event.seq.get() != boundary)
            {
                return Err(SessionError::Other(format!(
                    "fork boundary {boundary} does not match a contiguous event seq in session \"{source}\""
                )));
            }
            let seed = crate::fork::build_fork_seed(entry.session.events(), boundary)
                .map_err(|err| SessionError::Other(err.to_string()))?;
            (cwd, seed, boundary + 1)
        };
        let meta = SessionMeta {
            cwd,
            parent_session: Some(source.clone()),
            is_seeded: true,
            ..SessionMeta::default()
        };
        self.create(Some(child_id), &meta, seed, inherited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::seq;
    use fr_wire::event::TurnStartData;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<String>>>;

    fn make_store() -> (SessionStore, Log) {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        (
            SessionStore::new(
                Arc::new(|| 1_000),
                Arc::new(move |message| sink.lock().expect("warn lock").push(message)),
            ),
            log,
        )
    }

    fn meta() -> SessionMeta {
        SessionMeta::default()
    }

    fn turn_start_data(turn: u64) -> SessionEventData {
        SessionEventData::TurnStart(TurnStartData { turn })
    }

    #[test]
    fn create_mints_ids_and_rejects_duplicates() {
        let (mut store, _log) = make_store();
        let a = store
            .create(None, &meta(), Vec::new(), 0)
            .expect("create a");
        let b = store
            .create(None, &meta(), Vec::new(), 0)
            .expect("create b");
        assert_eq!(a.to_string(), "session-1");
        assert_eq!(b.to_string(), "session-2");
        let err = store
            .create(Some(SessionId::new("session-1")), &meta(), Vec::new(), 0)
            .expect_err("duplicate");
        assert_eq!(err.to_string(), "session \"session-1\" already exists");
    }

    #[test]
    fn announce_guards() {
        let (mut store, _log) = make_store();
        let id = store.create(None, &meta(), Vec::new(), 0).expect("create");
        let err = store.announce(&id).expect_err("double announce");
        assert_eq!(
            err.to_string(),
            "session \"session-1\" was already announced"
        );
        let missing = SessionId::new("nope");
        let err = store.announce(&missing).expect_err("not live");
        assert_eq!(
            err.to_string(),
            "session \"nope\" is not live in this store"
        );
    }

    #[test]
    fn created_veto_rolls_back_with_disposal() {
        let (mut store, _) = make_store();
        let disposed: Log = Arc::new(Mutex::new(Vec::new()));
        let d = Arc::clone(&disposed);
        store.subscribe_created(Arc::new(|_| Err("no".to_owned())));
        store.subscribe_disposed(Arc::new(move |id| {
            d.lock().expect("d lock").push(id.to_string());
            Ok(())
        }));
        let err = store
            .create(None, &meta(), Vec::new(), 0)
            .expect_err("veto");
        assert_eq!(err.to_string(), "no");
        assert_eq!(disposed.lock().expect("d lock").as_slice(), ["session-1"]);
        assert!(store.get(&SessionId::new("session-1")).is_none());
    }

    #[test]
    fn detach_without_announce_has_no_disposal() {
        let (mut store, _) = make_store();
        let disposed: Log = Arc::new(Mutex::new(Vec::new()));
        let d = Arc::clone(&disposed);
        store.subscribe_disposed(Arc::new(move |id| {
            d.lock().expect("d lock").push(id.to_string());
            Ok(())
        }));
        let session = store
            .prepare(Some(SessionId::new("p")), &meta(), Vec::new(), 0)
            .expect("prepare");
        let mut handle = store.enter(session).expect("enter");
        store.detach(&mut handle);
        assert!(store.get(&SessionId::new("p")).is_none());
        assert!(disposed.lock().expect("d lock").is_empty());
    }

    #[test]
    fn event_feed_contains_errors_keeps_order_and_grows_log() {
        let (mut store, log) = make_store();
        let seen: Log = Arc::new(Mutex::new(Vec::new()));
        let id = store.create(None, &meta(), Vec::new(), 0).expect("create");
        let s1 = Arc::clone(&seen);
        store.subscribe_event(Arc::new(move |_, _| {
            s1.lock().expect("s lock").push("one".to_owned());
            Err("boom".to_owned())
        }));
        let s2 = Arc::clone(&seen);
        store.subscribe_event(Arc::new(move |_, event| {
            s2.lock()
                .expect("s lock")
                .push(format!("two@{}", event.seq.get()));
            Ok(())
        }));
        store
            .append_log_only(&id, 5, turn_start_data(1))
            .expect("append");
        assert_eq!(seen.lock().expect("s lock").as_slice(), ["one", "two@0"]);
        assert_eq!(
            log.lock().expect("log lock").as_slice(),
            ["session \"session-1\": session/event listener threw: boom"]
        );
        assert_eq!(store.get(&id).expect("live").events().len(), 1);
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let (mut store, _log) = make_store();
        let seen: Log = Arc::new(Mutex::new(Vec::new()));
        let id = store.create(None, &meta(), Vec::new(), 0).expect("create");
        let s = Arc::clone(&seen);
        let sub = store.subscribe_event(Arc::new(move |_, event| {
            s.lock().expect("s lock").push(event.seq.get().to_string());
            Ok(())
        }));
        assert!(store.unsubscribe(sub));
        store
            .append_log_only(&id, 5, turn_start_data(1))
            .expect("append");
        assert!(seen.lock().expect("s lock").is_empty());
        assert!(!store.unsubscribe(sub));
    }

    #[test]
    fn flush_participation_and_first_registered_failure() {
        let (mut store, _) = make_store();
        let id = store.create(None, &meta(), Vec::new(), 0).expect("create");
        assert!(!store.flush(&id).expect("no listeners"));
        store.subscribe_flush(Arc::new(|_| Err("a".to_owned())));
        store.subscribe_flush(Arc::new(|_| Err("b".to_owned())));
        let err = store.flush(&id).expect_err("first failure");
        assert_eq!(err.to_string(), "a");
        store.subscribe_flush(Arc::new(|_| Ok(())));
        let err = store.flush(&id).expect_err("first failure still");
        assert_eq!(err.to_string(), "a");
        let (mut other, _) = make_store();
        let id2 = other
            .create(None, &meta(), Vec::new(), 0)
            .expect("create 2");
        other.subscribe_flush(Arc::new(|_| Ok(())));
        assert!(other.flush(&id2).expect("ok flush"));
        let missing = SessionId::new("zz");
        assert_eq!(
            other.flush(&missing).expect_err("not live").to_string(),
            "session \"zz\" is not live in this store"
        );
    }

    #[test]
    fn detach_is_single_shot_and_removes_entry() {
        let (mut store, _) = make_store();
        let session = store
            .prepare(Some(SessionId::new("a")), &meta(), Vec::new(), 0)
            .expect("prepare");
        let mut handle = store.enter(session).expect("enter");
        store.detach(&mut handle);
        assert!(store.get(&SessionId::new("a")).is_none());
        store.detach(&mut handle);
        assert!(store.get(&SessionId::new("a")).is_none());
    }

    #[test]
    fn fork_rejections_and_child_header() {
        let (mut store, _log) = make_store();
        let s1 = store
            .create(
                Some(SessionId::new("s1")),
                &SessionMeta {
                    cwd: Some("/tmp/w".to_owned()),
                    ..meta()
                },
                Vec::new(),
                0,
            )
            .expect("create s1");
        store
            .append_log_only(&s1, 1, turn_start_data(1))
            .expect("append");
        store
            .append_log_only(&s1, 2, turn_start_data(2))
            .expect("append");
        let err = store
            .fork(&SessionId::new("zz"), SessionId::new("c0"), None)
            .expect_err("unknown source");
        assert_eq!(err.to_string(), "session \"zz\" is not live in this store");
        store
            .create(Some(SessionId::new("c2")), &meta(), Vec::new(), 0)
            .expect("create c2");
        let err = store
            .fork(&s1, SessionId::new("c2"), Some(0))
            .expect_err("child exists");
        assert_eq!(err.to_string(), "session \"c2\" already exists");
        let err = store
            .fork(&s1, SessionId::new("c3"), Some(5))
            .expect_err("boundary beyond");
        assert_eq!(
            err.to_string(),
            "fork boundary 5 does not exist in session \"s1\" (last seq: 1)"
        );
        let child = store
            .fork(&s1, SessionId::new("c1"), Some(0))
            .expect("fork");
        let child_session = store.get(&child).expect("child live");
        assert_eq!(child.to_string(), "c1");
        let header = child_session.header();
        assert_eq!(
            header
                .parent_session
                .as_ref()
                .map(SessionId::to_string)
                .as_deref(),
            Some("s1")
        );
        assert_eq!(header.cwd.as_deref(), Some("/tmp/w"));
        assert!(header.is_seeded);
        assert_eq!(child_session.events().len(), 3);
        let child2 = store
            .fork(&s1, SessionId::new("c4"), None)
            .expect("fork default");
        assert_eq!(store.get(&child2).expect("live").events().len(), 4);
        let empty_id = store
            .create(Some(SessionId::new("e")), &meta(), Vec::new(), 0)
            .expect("create e");
        let child3 = store
            .fork(&empty_id, SessionId::new("c5"), None)
            .expect("fork empty");
        let empty_child = store.get(&child3).expect("live");
        assert!(empty_child.header().is_seeded);
        assert_eq!(empty_child.events().len(), 0);
    }

    #[test]
    fn create_with_seed_replays_history() {
        let (mut store, _) = make_store();
        let seed = vec![SessionEvent {
            seq: seq(0),
            time: 7,
            data: turn_start_data(1),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }];
        let id = store
            .create(Some(SessionId::new("r")), &meta(), seed, 0)
            .expect("create");
        assert_eq!(store.get(&id).expect("live").events().len(), 1);
    }

    #[test]
    fn prepare_rejects_relative_cwd() {
        let (mut store, _) = make_store();
        let relative = SessionMeta {
            cwd: Some("rel".to_owned()),
            ..meta()
        };
        let err = store
            .create(Some(SessionId::new("x")), &relative, Vec::new(), 0)
            .expect_err("relative cwd");
        assert_eq!(err.to_string(), "meta.cwd must be an absolute path");
    }
}
