// Rust guideline compliant 2026-10-27
//! The durable inbox face (dsh `ReactLoopInbox` port).
//!
//! Every mutation rides one `agent/inbox/spliced` append through the
//! session service, and the local fold replays the *committed* event (the
//! reply's seq is the authority), so the live path and the restore path
//! share one source of truth. Live lifecycle events (`agent/inbox/
//! inserted`, `agent/inbox/discarded`, `agent/inbox/claimed`) publish
//! through the agent-event face; insertion events read the committed
//! event's data, never the caller's input (dsh reads `event.data.inserted`).
//!
//! Claim semantics (dsh `claim`): the complete next-step batch first,
//! then — when the boundary consumes a queued turn — at most one
//! next-turn message; the combined batch is reported in that order.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use fr_wire::event::SessionEventData;
use fr_wire::ids::MessageId;
use fr_wire::message::Message;

use crate::agent_events::{AgentEvents, InboxClaimedPayload, InboxMessagePayload};
use crate::inbox::{
    InboxFoldError, InboxSplice, InboxSpliceError, InboxState, InboxTarget, fold_inbox, plan_splice,
};
use crate::session_client::{SessionClient, SessionClientError};

/// The durable inbox mutation event type.
pub const EVENT_INBOX_SPLICED: &str = "agent/inbox/spliced";

/// One failed inbox mutation.
#[derive(Debug)]
pub enum InboxError {
    /// The splice would duplicate a pending id.
    Plan(InboxSpliceError),
    /// The committed event failed validation on replay.
    Fold(InboxFoldError),
    /// The session service rejected the append.
    Session(SessionClientError),
    /// The committed event's data is not the splice payload shape.
    SpliceDecode(String),
}

impl fmt::Display for InboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(error) => write!(f, "{error}"),
            Self::Fold(error) => write!(f, "{error}"),
            Self::Session(error) => write!(f, "{error}"),
            Self::SpliceDecode(text) => write!(f, "{text}"),
        }
    }
}

impl Error for InboxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Plan(error) => Some(error),
            Self::Fold(error) => Some(error),
            Self::Session(error) => Some(error),
            Self::SpliceDecode(_) => None,
        }
    }
}

/// One session's durable inbox over the session service.
#[derive(Debug)]
pub struct LoopInbox {
    client: SessionClient,
    events: Arc<AgentEvents>,
    state: InboxState,
}

impl LoopInbox {
    /// Builds the inbox over the session client and event face.
    #[must_use]
    pub fn new(client: SessionClient, events: Arc<AgentEvents>) -> Self {
        Self {
            client,
            events,
            state: InboxState::default(),
        }
    }

    /// Restores the inbox by folding the committed event log (dsh restores
    /// through the same projection the live mutations write). The fold is
    /// silent: restore emits no lifecycle events.
    ///
    /// # Errors
    ///
    /// Returns the first splice payload or fold failure in the log.
    pub fn restored(
        client: SessionClient,
        events: Arc<AgentEvents>,
        log: &[fr_wire::event::SessionEvent],
    ) -> Result<Self, InboxError> {
        let mut state = InboxState::default();
        for event in log {
            if event.event_type() != EVENT_INBOX_SPLICED {
                continue;
            }
            let committed = committed_splice(event).map_err(InboxError::SpliceDecode)?;
            state = fold_inbox(&state, event.seq.get(), &committed).map_err(InboxError::Fold)?;
        }
        Ok(Self {
            client,
            events,
            state,
        })
    }

    /// The bound session id.
    #[must_use]
    pub fn session_id(&self) -> &str {
        self.client.session_id()
    }

    /// The current fold state.
    #[must_use]
    pub const fn state(&self) -> &InboxState {
        &self.state
    }

    /// Whether either queue holds pending input.
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        !self.state.next_turn.is_empty() || !self.state.next_step.is_empty()
    }

    /// The pending next-step batch.
    #[must_use]
    pub fn next_step(&self) -> &[Message] {
        &self.state.next_step
    }

    /// The pending next-turn queue.
    #[must_use]
    pub fn next_turn(&self) -> &[Message] {
        &self.state.next_turn
    }

    /// Durably cancels all pending input, clearing next-step before
    /// next-turn (dsh `clear`).
    ///
    /// # Errors
    ///
    /// Returns the first splice failure.
    pub fn clear(&mut self) -> Result<(), InboxError> {
        let step_len = queue_len(self, InboxTarget::NextStep);
        self.splice(InboxTarget::NextStep, 0, step_len, Vec::new())?;
        let turn_len = queue_len(self, InboxTarget::NextTurn);
        self.splice(InboxTarget::NextTurn, 0, turn_len, Vec::new())?;
        Ok(())
    }

    /// Removes and returns the batch proposed for one step boundary: the
    /// complete next-step queue, then at most one next-turn message when
    /// the boundary consumes a queued turn (dsh `claim`).
    ///
    /// # Errors
    ///
    /// Returns the first splice failure.
    pub fn claim(&mut self, target: InboxTarget, turn: u64) -> Result<Vec<Message>, InboxError> {
        let step_len = queue_len(self, InboxTarget::NextStep);
        let mut claimed = self.mutate(InboxTarget::NextStep, 0, step_len, Vec::new(), false)?;
        if target == InboxTarget::NextTurn {
            claimed.extend(self.mutate(InboxTarget::NextTurn, 0, 1, Vec::new(), false)?);
        }
        for message in &claimed {
            self.events.emit_inbox_claimed(&InboxClaimedPayload {
                message: message.clone(),
                turn,
            });
        }
        Ok(claimed)
    }

    /// Inserts one message at the head of a pending list (dsh `prepend`).
    ///
    /// # Errors
    ///
    /// Returns the splice failure.
    pub fn prepend(&mut self, target: InboxTarget, message: Message) -> Result<(), InboxError> {
        self.splice(target, 0, 0, vec![message])?;
        Ok(())
    }

    /// Replaces one pending message in place, searching next-turn before
    /// next-step (dsh `replace`); returns whether the id was still
    /// pending.
    ///
    /// # Errors
    ///
    /// Returns the splice failure.
    pub fn replace(
        &mut self,
        message_id: &MessageId,
        new_message: Message,
    ) -> Result<bool, InboxError> {
        match self.locate(message_id) {
            Some((target, index)) => {
                self.splice(target, index, 1, vec![new_message])?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Removes one pending message, searching next-turn before next-step
    /// (dsh `remove`); returns whether the id was still pending.
    ///
    /// # Errors
    ///
    /// Returns the splice failure.
    pub fn remove(&mut self, message_id: &MessageId) -> Result<bool, InboxError> {
        match self.locate(message_id) {
            Some((target, index)) => {
                self.splice(target, index, 1, Vec::new())?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Where one id pends, next-turn first (dsh `locate`).
    fn locate(&self, message_id: &MessageId) -> Option<(InboxTarget, i64)> {
        for (target, list) in [
            (InboxTarget::NextTurn, &self.state.next_turn),
            (InboxTarget::NextStep, &self.state.next_step),
        ] {
            if let Some(index) = list
                .iter()
                .position(|message| crate::inbox::message_id(message) == Some(message_id))
            {
                let index = i64::try_from(index)
                    .expect("an in-memory list length fits i64 on every supported target");
                return Some((target, index));
            }
        }
        None
    }

    /// Applies standard splice semantics with the removed messages
    /// discarded (dsh `splice` = `mutate(..., true)`).
    ///
    /// # Errors
    ///
    /// Returns the splice failure.
    pub fn splice(
        &mut self,
        target: InboxTarget,
        start: i64,
        delete_count: i64,
        inserted: Vec<Message>,
    ) -> Result<Vec<Message>, InboxError> {
        self.mutate(target, start, delete_count, inserted, true)
    }

    /// Commits one normalized mutation and publishes its live events (dsh
    /// `mutate`): plan, append, fold the committed event, emit.
    ///
    /// # Errors
    ///
    /// Returns the planning, transport, or fold failure.
    fn mutate(
        &mut self,
        target: InboxTarget,
        start: i64,
        delete_count: i64,
        inserted: Vec<Message>,
        discard_removed: bool,
    ) -> Result<Vec<Message>, InboxError> {
        let Some((payload, removed)) = plan_splice(
            &self.state,
            target,
            start,
            delete_count,
            inserted,
            discard_removed,
        )
        .map_err(InboxError::Plan)?
        else {
            return Ok(Vec::new());
        };
        let data = serde_json::to_value(&payload)
            .map_err(|error| InboxError::SpliceDecode(error.to_string()))?;
        let event = self
            .client
            .append(EVENT_INBOX_SPLICED, &data, None, None)
            .map_err(InboxError::Session)?;
        let committed = committed_splice(&event).map_err(InboxError::SpliceDecode)?;
        self.state =
            fold_inbox(&self.state, event.seq.get(), &committed).map_err(InboxError::Fold)?;
        if discard_removed {
            for message in &removed {
                self.events.emit_inbox_discarded(&InboxMessagePayload {
                    message: message.clone(),
                });
            }
        }
        for message in &committed.inserted {
            self.events.emit_inbox_inserted(&InboxMessagePayload {
                message: message.clone(),
            });
        }
        Ok(removed)
    }
}

/// One queue's length in the splice argument domain.
fn queue_len(inbox: &LoopInbox, target: InboxTarget) -> i64 {
    i64::try_from(inbox.state().list(target).len())
        .expect("an in-memory list length fits i64 on every supported target")
}

/// Reads the committed splice payload back out of the appended event (the
/// insert events read the log, not the caller's input).
fn committed_splice(event: &fr_wire::event::SessionEvent) -> Result<InboxSplice, String> {
    let SessionEventData::Unknown(unknown) = &event.data else {
        return Err(format!(
            "inbox splice event \"{}\" carries typed {} data",
            EVENT_INBOX_SPLICED,
            event.event_type()
        ));
    };
    if unknown.type_name != EVENT_INBOX_SPLICED {
        return Err(format!(
            "inbox splice event carries type \"{}\"",
            unknown.type_name
        ));
    }
    serde_json::from_value(serde_json::Value::Object(unknown.data.clone()))
        .map_err(|error| format!("inbox splice event data is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{InboxError, LoopInbox};
    use crate::agent_events::AgentEvents;
    use crate::inbox::InboxTarget;
    use crate::outbound::Outbound;
    use crate::session_client::SessionClient;
    use dougong_abi::{HostExports, STATUS_OK};
    use fr_wire::message::Message;
    use std::ffi::c_void;
    use std::sync::{Arc, Mutex};

    /// A fake session provider over the documented append contract; it
    /// assigns seq/time and folds nothing (the fold under test is the
    /// inbox's own).
    struct FakeSession {
        next_seq: u64,
        requests: Vec<serde_json::Value>,
    }

    unsafe extern "C" fn serving_require(
        host: *mut c_void,
        _service: *const u8,
        _service_len: usize,
        _method: u32,
        in_data: *const u8,
        in_len: usize,
        out_buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> i32 {
        // SAFETY: the vtable contract passes a valid readable handle.
        let fake = unsafe { &mut *(host.cast::<FakeSession>()) };
        // SAFETY: `in_data` is valid for `in_len` readable bytes for this
        // call.
        let payload: serde_json::Value =
            serde_json::from_slice(unsafe { std::slice::from_raw_parts(in_data, in_len) })
                .expect("request is JSON");
        fake.requests.push(payload.clone());
        let seq = fake.next_seq;
        fake.next_seq += 1;
        let reply = serde_json::json!({
            "event": {
                "type": payload["type"],
                "seq": seq,
                "time": 1_700_000_000_000_i64,
                "data": payload["data"],
            }
        });
        let bytes = serde_json::to_vec(&reply).expect("reply serializes");
        // SAFETY: the vtable contract guarantees the caller-owned buffer.
        let written = unsafe { dougong_abi::write_out(&bytes, out_buf, cap, out_len) };
        if written {
            STATUS_OK
        } else {
            dougong_abi::STATUS_CAPACITY
        }
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

    struct Harness {
        inbox: LoopInbox,
        inserted: Arc<Mutex<Vec<String>>>,
        discarded: Arc<Mutex<Vec<String>>>,
        claimed: Arc<Mutex<Vec<(String, u64)>>>,
        fake: *mut FakeSession,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            // SAFETY: the inbox's requires returned synchronously; the box
            // is reclaimed exactly once.
            drop(unsafe { Box::from_raw(self.fake) });
        }
    }

    /// The stable identity of one known-shape message (fixture messages
    /// are always known-shape).
    fn message_id(message: &Message) -> String {
        match message {
            Message::Known(known) => known.id().as_str().to_owned(),
            Message::Other(_) => panic!("fixture messages are known-shape"),
        }
    }

    fn user_message(id: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "role": "user",
            "id": id,
            "content": [{ "type": "text", "text": "task text" }],
            "source": { "kind": "user" },
        }))
        .expect("fixture message")
    }

    fn harness() -> Harness {
        let fake = Box::into_raw(Box::new(FakeSession {
            next_seq: 1,
            requests: Vec::new(),
        }));
        let outbound = Outbound::new(HostExports {
            grant: crate::outbound::quiet_grant,
            host_handle: fake.cast::<c_void>(),
            require: serving_require,
            emit: quiet_emit,
        });
        let mut events = AgentEvents::new(outbound, Arc::new(|_warning: &str| {}));
        let inserted = Arc::new(Mutex::new(Vec::new()));
        let discarded = Arc::new(Mutex::new(Vec::new()));
        let claimed = Arc::new(Mutex::new(Vec::new()));
        let inserted_listener = Arc::clone(&inserted);
        events.on_inbox_inserted(Box::new(move |payload| {
            inserted_listener
                .lock()
                .expect("test lock")
                .push(message_id(&payload.message));
            Ok(())
        }));
        let discarded_listener = Arc::clone(&discarded);
        events.on_inbox_discarded(Box::new(move |payload| {
            discarded_listener
                .lock()
                .expect("test lock")
                .push(message_id(&payload.message));
            Ok(())
        }));
        let claimed_listener = Arc::clone(&claimed);
        events.on_inbox_claimed(Box::new(move |payload| {
            claimed_listener
                .lock()
                .expect("test lock")
                .push((message_id(&payload.message), payload.turn));
            Ok(())
        }));
        let events = Arc::new(events);
        let client = SessionClient::new(events.outbound(), "s1");
        let inbox = LoopInbox::new(client, Arc::clone(&events));
        Harness {
            inbox,
            inserted,
            discarded,
            claimed,
            fake,
        }
    }

    #[test]
    fn splice_appends_the_durable_event_and_emits_insertions() {
        let mut harness = harness();
        let removed = harness
            .inbox
            .splice(InboxTarget::NextStep, 0, 0, vec![user_message("m1")])
            .expect("splice");
        assert_eq!(removed, []);
        assert_eq!(harness.inbox.next_step(), &[user_message("m1")]);
        assert!(harness.inbox.has_pending());
        assert_eq!(*harness.inserted.lock().expect("test lock"), ["m1"]);
        assert_eq!(
            *harness.discarded.lock().expect("test lock"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn claim_takes_the_step_batch_then_one_turn_message() {
        let mut harness = harness();
        for id in ["s1", "s2"] {
            harness
                .inbox
                .splice(InboxTarget::NextStep, i64::MAX, 0, vec![user_message(id)])
                .expect("seed step queue");
        }
        harness
            .inbox
            .splice(InboxTarget::NextTurn, i64::MAX, 0, vec![user_message("t1")])
            .expect("seed turn queue");
        harness
            .inbox
            .splice(InboxTarget::NextTurn, i64::MAX, 0, vec![user_message("t2")])
            .expect("seed turn queue again");

        let claimed = harness
            .inbox
            .claim(InboxTarget::NextTurn, 1)
            .expect("claim");
        assert_eq!(
            claimed,
            [user_message("s1"), user_message("s2"), user_message("t1")]
        );
        assert_eq!(
            *harness.claimed.lock().expect("test lock"),
            [
                ("s1".to_owned(), 1_u64),
                ("s2".to_owned(), 1),
                ("t1".to_owned(), 1),
            ]
        );
        assert_eq!(harness.inbox.next_step(), []);
        assert_eq!(harness.inbox.next_turn(), &[user_message("t2")]);

        let claimed = harness
            .inbox
            .claim(InboxTarget::NextStep, 1)
            .expect("claim again");
        assert_eq!(claimed, []);
        assert_eq!(harness.inbox.next_turn(), &[user_message("t2")]);
    }

    #[test]
    fn clear_discards_step_queue_before_turn_queue() {
        let mut harness = harness();
        harness
            .inbox
            .splice(InboxTarget::NextStep, 0, 0, vec![user_message("s1")])
            .expect("seed step queue");
        harness
            .inbox
            .splice(InboxTarget::NextTurn, 0, 0, vec![user_message("t1")])
            .expect("seed turn queue");
        let requests_before = request_count(harness.fake);
        harness.inbox.clear().expect("clear");
        assert!(!harness.inbox.has_pending());
        assert_eq!(*harness.discarded.lock().expect("test lock"), ["s1", "t1"]);
        // Two durable splices: next-step first, then next-turn.
        assert_eq!(request_count(harness.fake) - requests_before, 2);
    }

    #[test]
    fn duplicate_ids_across_queues_refuse_the_splice() {
        let mut harness = harness();
        harness
            .inbox
            .splice(InboxTarget::NextStep, 0, 0, vec![user_message("dup")])
            .expect("seed step queue");
        let error = harness
            .inbox
            .splice(InboxTarget::NextTurn, 0, 0, vec![user_message("dup")])
            .expect_err("duplicate id refuses");
        assert!(matches!(error, InboxError::Plan(_)));
        assert!(error.to_string().contains("already pending"));
    }

    #[test]
    fn prepend_inserts_at_the_queue_head() {
        let mut harness = harness();
        harness
            .inbox
            .splice(InboxTarget::NextStep, i64::MAX, 0, vec![user_message("s1")])
            .expect("seed step queue");
        harness
            .inbox
            .prepend(InboxTarget::NextStep, user_message("head"))
            .expect("prepend");
        assert_eq!(
            harness.inbox.next_step(),
            &[user_message("head"), user_message("s1")]
        );
    }

    #[test]
    fn replace_and_remove_locate_next_turn_before_next_step() {
        let mut harness = harness();
        harness
            .inbox
            .splice(InboxTarget::NextTurn, 0, 0, vec![user_message("t1")])
            .expect("seed turn queue");
        harness
            .inbox
            .splice(InboxTarget::NextStep, 0, 0, vec![user_message("s1")])
            .expect("seed step queue");
        // Both queues would match by position; the id decides, and the
        // turn queue is searched first (dsh `locate`).
        let replaced = harness
            .inbox
            .replace(&message_id_of(&user_message("t1")), user_message("t2"))
            .expect("replace");
        assert!(replaced);
        assert_eq!(harness.inbox.next_turn(), &[user_message("t2")]);
        assert_eq!(harness.inbox.next_step(), &[user_message("s1")]);
        let removed = harness
            .inbox
            .remove(&message_id_of(&user_message("s1")))
            .expect("remove");
        assert!(removed);
        assert_eq!(harness.inbox.next_step(), []);
        let missing = harness
            .inbox
            .remove(&message_id_of(&user_message("gone")))
            .expect("remove");
        assert!(!missing);
    }

    fn message_id_of(message: &Message) -> fr_wire::ids::MessageId {
        match message {
            Message::Known(known) => known.id().clone(),
            Message::Other(_) => panic!("fixture messages are known-shape"),
        }
    }

    #[test]
    fn no_op_splices_never_touch_the_log() {
        let mut harness = harness();
        let requests_before = request_count(harness.fake);
        let removed = harness
            .inbox
            .splice(InboxTarget::NextStep, 0, 0, Vec::new())
            .expect("no-op splice");
        assert_eq!(removed, []);
        assert_eq!(request_count(harness.fake), requests_before);
    }

    fn request_count(fake: *mut FakeSession) -> usize {
        // SAFETY: the tests run single-threaded; the box stays live for the
        // harness's lifetime.
        unsafe { (*fake).requests.len() }
    }
}
