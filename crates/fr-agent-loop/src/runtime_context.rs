// Rust guideline compliant 2026-10-27
//! Durable projection state for the two loop-owned surface messages the
//! system-prompt plugin forms.
//!
//! Those two messages are the system prompt (surface node 0 and any
//! in-history replacement) and the dynamic runtime-context snapshot.
//! Mirrors dsh `agent-loop/src/runtime-context.ts`: neither projection owns
//! its commit — they decide what should reach the surface and hand the
//! driver uncommitted messages with their surface intents. The
//! `runtime-context` message-source kind rides fr-wire's merge-extensible
//! `Other` fallback (dsh declares it into the source map at compile time);
//! fr-session admission accepts any source on the user role.

use std::collections::HashSet;

use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject};
use fr_wire::event::{SessionEvent, SessionEventData, SurfaceOp};
use fr_wire::ids::{MessageId, SessionSeq};
use fr_wire::message::{ContextSnapshotSection, KnownMessage, Message, MessageSource};
use serde::{Deserialize, Serialize};

/// The message-source kind owning runtime-context snapshots.
pub const SOURCE_KIND: &str = "runtime-context";

/// The snapshot text that supersedes earlier runtime-context snapshots.
pub const CLEARED_MARKER: &str =
    "Current runtime context: none. Earlier runtime-context snapshots no longer apply.";

/// How one system-prompt decision reaches the surface (dsh
/// `SurfaceIntent<'system/message'>` over fr-wire primitives): an append of
/// a new system node, or a replacement of one surviving system node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPromptIntent {
    /// How the event joins the surface.
    pub surface_op: SurfaceOp,
    /// Earlier source-event seqs the replacement cites; absent on appends.
    pub source_event_seqs: Option<Vec<SessionSeq>>,
}

/// One uncommitted system-prompt surface operation for request admission or
/// reconciliation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemPromptCommit {
    /// Rendered prompt or empty content: an empty head records no prompt;
    /// empty tails are dormant.
    pub message: Message,
    /// Append for a new system node, otherwise a replacement of one
    /// surviving node.
    pub intent: SystemPromptIntent,
}

/// The request-series facts one prompt decision is made under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemPromptDecisionInput {
    /// Whether the prepared route for this attempt reads a later `system`
    /// message as the effective prompt.
    pub in_history: bool,
    /// Whether prompt admission must consolidate: a pre-step listener
    /// declared a new series, the surface changed since the last request, or
    /// assembled tools changed on a route without tool-update support.
    pub starts_series: bool,
}

/// One surviving `system/message` surface node with its effective text.
///
/// `text` is `None` when the node carries no single text block (dsh
/// `undefined`), `Some("")` for empty content, and `Some(text)` for a
/// single text block — the distinction decides dormancy and freshness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptNode {
    /// The surface event's sequence number.
    pub seq: SessionSeq,
    /// The node's effective text, tri-stated as the dsh projection reads it.
    pub text: Option<String>,
}

/// The last retained runtime-context snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedSnapshot {
    /// The surface event holding the snapshot.
    pub seq: SessionSeq,
    /// The snapshot text; `None` when the message carries no single text
    /// block (never equal to any rendered snapshot).
    pub text: Option<String>,
}

/// `retained` tri-state (dsh `undefined | null | {seq, text}`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Retention {
    /// No snapshot ever existed.
    Never,
    /// None is retained (an owned snapshot exists but left the surface, or
    /// never reached it).
    Cleared,
    /// The snapshot currently on the surface.
    Held(RetainedSnapshot),
}

/// The exact single text block of a message, or `None` for any other
/// content shape (dsh `textOf`).
fn text_of(message: &Message) -> Option<String> {
    let Message::Known(known) = message else {
        return None;
    };
    let content = known.content();
    if content.len() != 1 {
        return None;
    }
    match &content[0] {
        ContentBlock::Known(KnownContentBlock::Text { text }) => Some(text.clone()),
        _ => None,
    }
}

/// The effective text of one system node: empty content reads as the empty
/// string, otherwise the strict single-text read (dsh `systemNodes`).
fn node_text(message: &Message) -> Option<String> {
    let Message::Known(known) = message else {
        return None;
    };
    if known.content().is_empty() {
        Some(String::new())
    } else {
        text_of(message)
    }
}

/// Whether a message is owned by the runtime-context projection (dsh
/// `isOwned`): the source kind rides the merge-extensible fallback.
fn is_owned(message: &Message) -> bool {
    let Message::Known(known) = message else {
        return false;
    };
    match known.source() {
        MessageSource::Other(object) => {
            object.get("kind").and_then(serde_json::Value::as_str) == Some(SOURCE_KIND)
        }
        MessageSource::Known(_) => false,
    }
}

/// Whether an event replaced a surface range (dsh
/// `isReplacementSurfaceEvent`: a surface event whose op is not append).
const fn is_replacement_surface_event(event: &SessionEvent) -> bool {
    matches!(event.surface_op, Some(SurfaceOp::Replace(_)))
}

/// Builds one identified system message (dsh `createSystemMessage`).
///
/// Empty text records "no system prompt" as empty content; the source is
/// always the system-prompt kind. Identity is injected rather than
/// generated — the id seam matches the fr-session clock seam.
#[must_use]
pub fn create_system_message(id: MessageId, text: &str) -> Message {
    let content = if text.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::Known(KnownContentBlock::Text {
            text: text.to_owned(),
        })]
    };
    Message::Known(Box::new(KnownMessage::System {
        id,
        content,
        source: MessageSource::Known(fr_wire::message::KnownMessageSource::SystemPrompt {
            extra: RawObject::new(),
        }),
        extra: RawObject::new(),
    }))
}

/// Builds one identified runtime-context snapshot user message (dsh
/// `createUserMessage` with the runtime-context source): a cleared snapshot
/// carries no contributions left to attribute, so its source drops the
/// `form`/`sections` fields.
fn create_snapshot_message(
    id: MessageId,
    snapshot: &str,
    sections: &[ContextSnapshotSection],
) -> Message {
    let mut source = RawObject::new();
    source.insert("kind".to_owned(), serde_json::json!(SOURCE_KIND));
    if !sections.is_empty() {
        source.insert("form".to_owned(), serde_json::json!("snapshot"));
        source.insert(
            "sections".to_owned(),
            serde_json::to_value(sections).expect("section wire shape serializes"),
        );
    }
    Message::Known(Box::new(KnownMessage::User {
        id,
        content: vec![ContentBlock::Known(KnownContentBlock::Text {
            text: snapshot.to_owned(),
        })],
        source: MessageSource::Other(source),
        extra: RawObject::new(),
    }))
}

/// Collects the surviving `system/message` nodes in surface order (dsh
/// `systemNodes`): the caller resolves each surface seq to its event; a
/// non-system event at a resolved position contributes nothing.
#[must_use]
pub fn system_prompt_nodes<'a, I>(surface: I) -> Vec<SystemPromptNode>
where
    I: IntoIterator<Item = (SessionSeq, &'a SessionEvent)>,
{
    surface
        .into_iter()
        .filter_map(|(seq, event)| {
            let SessionEventData::SystemMessage(data) = &event.data else {
                return None;
            };
            Some(SystemPromptNode {
                seq,
                text: node_text(&data.message),
            })
        })
        .collect()
}

fn append_commit(id: MessageId, text: &str) -> SystemPromptCommit {
    SystemPromptCommit {
        message: create_system_message(id, text),
        intent: SystemPromptIntent {
            surface_op: SurfaceOp::Append(fr_wire::event::AppendOp),
            source_event_seqs: None,
        },
    }
}

fn replace_commit(id: MessageId, seq: SessionSeq, text: &str) -> SystemPromptCommit {
    SystemPromptCommit {
        message: create_system_message(id, text),
        intent: SystemPromptIntent {
            surface_op: SurfaceOp::Replace(fr_wire::event::ReplaceOp {
                op: fr_wire::event::ReplaceTag,
                start_seq: seq,
                end_seq: seq,
            }),
            source_event_seqs: Some(vec![seq]),
        },
    }
}

/// Decides how a rendered system prompt reaches the surface without owning
/// the commit (dsh `SystemPromptProjection.project`).
///
/// The first prompt, even empty, reserves surface node 0; a capable
/// continuing series appends changed nonempty text after the cached history;
/// an incapable route, broken series, or cleared prompt instead normalizes
/// the first system node and empties later active nodes. Dormant empty tails
/// supply no effective text and need no repeated replacements.
///
/// `next_id` supplies each created message's stable identity (the dsh
/// helper generates a fresh uuid internally; the seam keeps the fold pure).
#[must_use]
pub fn project_system_prompt(
    nodes: &[SystemPromptNode],
    rendered: &str,
    input: SystemPromptDecisionInput,
    mut next_id: impl FnMut() -> MessageId,
) -> Vec<SystemPromptCommit> {
    let Some(head) = nodes.first() else {
        return vec![append_commit(next_id(), rendered)];
    };
    let latest = nodes
        .iter()
        .rev()
        .find(|node| node.text.as_deref() != Some(""))
        .unwrap_or(head);
    if !input.in_history || input.starts_series || rendered.is_empty() {
        let mut updates: Vec<SystemPromptCommit> = nodes[1..]
            .iter()
            .filter(|node| node.text.as_deref() != Some(""))
            .map(|node| replace_commit(next_id(), node.seq, ""))
            .collect();
        if head.text.as_deref() != Some(rendered) {
            updates.push(replace_commit(next_id(), head.seq, rendered));
        }
        return updates;
    }
    if latest.text.as_deref() == Some(rendered) {
        return Vec::new();
    }
    vec![append_commit(next_id(), rendered)]
}

/// Tracks the last retained runtime-context snapshot without owning its
/// commit (dsh `RuntimeContextProjection`): restored once from the event
/// log, then driven by authoritative session events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeContextProjection {
    retained: Retention,
}

impl RuntimeContextProjection {
    /// Restores the projection state once from the event log, newest first
    /// (dsh constructor scan): the newest owned `user/message` on the
    /// surface is retained; an owned message that left the surface (or
    /// never reached it) clears retention; no owned message at all leaves
    /// the "never" state.
    #[must_use]
    pub fn restore<'a, I>(events_newest_first: I, surface: &HashSet<SessionSeq>) -> Self
    where
        I: IntoIterator<Item = &'a SessionEvent>,
    {
        let mut retained = Retention::Never;
        for event in events_newest_first {
            let SessionEventData::UserMessage(message) = &event.data else {
                continue;
            };
            if !is_owned(message) {
                continue;
            }
            if matches!(retained, Retention::Never) {
                retained = Retention::Cleared;
            }
            if surface.contains(&event.seq) {
                retained = Retention::Held(RetainedSnapshot {
                    seq: event.seq,
                    text: text_of(message),
                });
                break;
            }
        }
        Self { retained }
    }

    /// Follows one authoritative session event (dsh `session/event`
    /// listener): an owned user message becomes the retained snapshot; a
    /// replacement citing the retained snapshot's seq clears retention.
    pub fn observe(&mut self, event: &SessionEvent) {
        if let SessionEventData::UserMessage(message) = &event.data
            && is_owned(message)
        {
            self.retained = Retention::Held(RetainedSnapshot {
                seq: event.seq,
                text: text_of(message),
            });
            return;
        }
        if let Retention::Held(retained) = &self.retained {
            let cited = event
                .source_event_seqs
                .as_ref()
                .is_some_and(|seqs| seqs.contains(&retained.seq));
            if is_replacement_surface_event(event) && cited {
                self.retained = Retention::Cleared;
            }
        }
    }

    /// Creates an uncommitted snapshot only when the retained value differs
    /// (dsh `project`): nothing existed and nothing is current stays
    /// silent; an emptied context renders the cleared marker; an unchanged
    /// snapshot is not re-proposed.
    ///
    /// `next_id` supplies the message's stable identity.
    #[must_use]
    pub fn project(
        &self,
        current: &str,
        sections: &[ContextSnapshotSection],
        next_id: impl FnOnce() -> MessageId,
    ) -> Option<Message> {
        if matches!(self.retained, Retention::Never) && current.is_empty() {
            return None;
        }
        let snapshot = if current.is_empty() {
            CLEARED_MARKER
        } else {
            current
        };
        if let Retention::Held(retained) = &self.retained
            && retained.text.as_deref() == Some(snapshot)
        {
            return None;
        }
        Some(create_snapshot_message(next_id(), snapshot, sections))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLEARED_MARKER, RetainedSnapshot, RuntimeContextProjection, SOURCE_KIND,
        SystemPromptCommit, SystemPromptDecisionInput, SystemPromptNode, create_system_message,
        project_system_prompt, system_prompt_nodes,
    };
    use fr_wire::content::RawObject;
    use fr_wire::event::{
        AppendOp, ReplaceOp, ReplaceTag, SessionEvent, SessionEventData, SurfaceOp,
        SystemMessageData,
    };
    use fr_wire::ids::{MessageId, SessionSeq};
    use fr_wire::message::{
        ContextSnapshotSection, KnownMessage, KnownMessageSource, Message, MessageSource,
    };
    use serde_json::json;
    use std::collections::HashSet;

    fn id(n: u64) -> MessageId {
        MessageId::new(format!("m-{n}"))
    }

    fn session_seq(n: u64) -> SessionSeq {
        SessionSeq::new(n).expect("fixture seq values sit far below the safe-integer cap")
    }

    fn ids() -> impl FnMut() -> MessageId {
        let mut next = 0;
        move || {
            next += 1;
            id(next)
        }
    }

    fn node(seq: u64, text: Option<&str>) -> SystemPromptNode {
        SystemPromptNode {
            seq: session_seq(seq),
            text: text.map(str::to_owned),
        }
    }

    fn owned_user_message(seq_event: u64, text: &str) -> SessionEvent {
        let mut source = RawObject::new();
        source.insert("kind".to_owned(), json!(SOURCE_KIND));
        SessionEvent {
            seq: session_seq(seq_event),
            time: 0,
            data: SessionEventData::UserMessage(Message::Known(Box::new(KnownMessage::User {
                id: id(seq_event),
                content: vec![fr_wire::content::ContentBlock::Known(
                    fr_wire::content::KnownContentBlock::Text {
                        text: text.to_owned(),
                    },
                )],
                source: MessageSource::Other(source),
                extra: RawObject::new(),
            }))),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn plain_user_message() -> Message {
        Message::Known(Box::new(KnownMessage::User {
            id: id(99),
            content: vec![],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        }))
    }

    fn sections(names: &[&str]) -> Vec<ContextSnapshotSection> {
        names
            .iter()
            .map(|name| ContextSnapshotSection {
                name: (*name).to_owned(),
                text: format!("text of {name}"),
            })
            .collect()
    }

    fn assert_replace(commit: &SystemPromptCommit, seq: u64, text: &str) {
        assert_eq!(
            commit.intent.source_event_seqs,
            Some(vec![session_seq(seq)])
        );
        assert_eq!(
            commit.intent.surface_op,
            SurfaceOp::Replace(ReplaceOp {
                op: ReplaceTag,
                start_seq: session_seq(seq),
                end_seq: session_seq(seq),
            })
        );
        assert_eq!(
            commit.message,
            create_system_message(commit_message_id(commit), text)
        );
    }

    fn commit_message_id(commit: &SystemPromptCommit) -> MessageId {
        match &commit.message {
            Message::Known(known) => known.id().clone(),
            Message::Other(_) => panic!("commit messages keep their known shape"),
        }
    }

    fn assert_append(commit: &SystemPromptCommit, text: &str) {
        assert_eq!(commit.intent.source_event_seqs, None);
        assert_eq!(commit.intent.surface_op, SurfaceOp::Append(AppendOp));
        assert_eq!(
            commit.message,
            create_system_message(commit_message_id(commit), text)
        );
    }

    /// Covers the head-absent arm (the first prompt reserves surface node 0
    /// even when empty) and the created message wire shape.
    #[test]
    fn first_prompt_reserves_node_zero_even_empty() {
        let commits = project_system_prompt(&[], "", input(true, false), ids());
        assert_eq!(commits.len(), 1);
        assert_append(&commits[0], "");
        let Message::Known(known) = &commits[0].message else {
            panic!("commit messages keep their known shape");
        };
        assert_eq!(known.content(), [] as [fr_wire::content::ContentBlock; 0]);

        let commits = project_system_prompt(&[], "prompt", input(false, false), ids());
        assert_append(&commits[0], "prompt");
    }

    /// Covers the normalization arm: an incapable route (or broken series,
    /// or cleared prompt) empties later active nodes and rewrites the head;
    /// dormant empty tails are left alone; an already-matching head is not
    /// rewritten while its successors still normalize.
    #[test]
    fn normalization_empties_tail_and_rewrites_head() {
        let nodes = vec![
            node(1, Some("old")),
            node(2, Some("")),
            node(3, Some("stale")),
            node(4, None),
        ];
        let commits = project_system_prompt(&nodes, "new", input(false, false), ids());
        assert_eq!(commits.len(), 3);
        assert_replace(&commits[0], 3, "");
        assert_replace(&commits[1], 4, "");
        assert_replace(&commits[2], 1, "new");

        // A matching head avoids its own rewrite but still empties the tail.
        let nodes = vec![node(1, Some("same")), node(3, Some("stale"))];
        let commits = project_system_prompt(&nodes, "same", input(false, false), ids());
        assert_eq!(commits.len(), 1);
        assert_replace(&commits[0], 3, "");

        // A cleared prompt also normalizes (rendered empty).
        let nodes = vec![node(1, Some("old")), node(2, Some("stale"))];
        let commits = project_system_prompt(&nodes, "", input(true, false), ids());
        assert_eq!(commits.len(), 2);
        assert_replace(&commits[0], 2, "");
        assert_replace(&commits[1], 1, "");
    }

    /// Covers the incremental arm: a capable continuing series appends
    /// changed text after the cached history, and an unchanged latest text
    /// commits nothing.
    #[test]
    fn capable_series_appends_changed_text() {
        let nodes = vec![node(1, Some("base")), node(2, Some("latest"))];
        let commits = project_system_prompt(&nodes, "latest", input(true, false), ids());
        assert_eq!(commits, [] as [SystemPromptCommit; 0]);

        let commits = project_system_prompt(&nodes, "fresh", input(true, false), ids());
        assert_eq!(commits.len(), 1);
        assert_append(&commits[0], "fresh");

        // A series restart consolidates even on a capable route.
        let commits = project_system_prompt(&nodes, "latest", input(true, true), ids());
        assert_eq!(commits.len(), 2);
        assert_replace(&commits[0], 2, "");
        assert_replace(&commits[1], 1, "latest");
    }

    /// Covers the node collection from surface events: non-system events
    /// are skipped, empty content reads as the empty string, and a
    /// multi-block node reads as `None`.
    #[test]
    fn system_nodes_read_tri_state_text() {
        let system_event = |seq: u64, content: Vec<fr_wire::content::ContentBlock>| SessionEvent {
            seq: session_seq(seq),
            time: 0,
            data: SessionEventData::SystemMessage(SystemMessageData {
                turn: 1,
                step: 1,
                message: Message::Known(Box::new(KnownMessage::System {
                    id: id(seq),
                    content,
                    source: MessageSource::Known(KnownMessageSource::SystemPrompt {
                        extra: RawObject::new(),
                    }),
                    extra: RawObject::new(),
                })),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let text_block = |text: &str| {
            fr_wire::content::ContentBlock::Known(fr_wire::content::KnownContentBlock::Text {
                text: text.to_owned(),
            })
        };
        let user_event = SessionEvent {
            seq: session_seq(0),
            time: 0,
            data: SessionEventData::UserMessage(plain_user_message()),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let nodes = system_prompt_nodes([
            (session_seq(0), &user_event),
            (session_seq(1), &system_event(1, vec![text_block("head")])),
            (session_seq(2), &system_event(2, vec![])),
            (
                session_seq(3),
                &system_event(3, vec![text_block("a"), text_block("b")]),
            ),
        ]);
        assert_eq!(
            nodes,
            vec![node(1, Some("head")), node(2, Some("")), node(3, None),]
        );
    }

    /// Covers the restore scan: newest first, an owned snapshot off the
    /// surface clears retention, a surface hit retains and stops, and no
    /// owned message at all leaves the "never" state that silences an empty
    /// current context.
    #[test]
    fn restore_scans_newest_first() {
        let mut surface = HashSet::new();
        surface.insert(session_seq(4));
        let events = [
            owned_user_message(2, "older"),
            owned_user_message(4, "newest"),
        ];
        let projection = RuntimeContextProjection::restore(events.iter().rev(), &surface);
        let message = projection
            .project("changed", &sections(&["cwd"]), || id(1))
            .expect("a changed snapshot is proposed");
        assert_eq!(
            message,
            snapshot_message_of(id(1), "changed", &sections(&["cwd"]))
        );

        // The retained snapshot matches: no proposal.
        let projection = RuntimeContextProjection::restore(events.iter().rev(), &surface);
        assert!(projection.project("newest", &[], || id(2)).is_none());

        // Newest owned message left the surface: retention clears.
        let empty_surface = HashSet::new();
        let projection = RuntimeContextProjection::restore(events.iter().rev(), &empty_surface);
        let message = projection
            .project("", &[], || id(3))
            .expect("a cleared marker is proposed");
        assert_eq!(message, snapshot_message_of(id(3), CLEARED_MARKER, &[]));

        // Never retained plus empty current: silent.
        let projection =
            RuntimeContextProjection::restore(std::iter::empty::<&SessionEvent>(), &empty_surface);
        assert!(projection.project("", &[], || id(4)).is_none());
    }

    fn snapshot_message_of(
        id: MessageId,
        text: &str,
        sections: &[ContextSnapshotSection],
    ) -> Message {
        let mut source = RawObject::new();
        source.insert("kind".to_owned(), json!(SOURCE_KIND));
        if !sections.is_empty() {
            source.insert("form".to_owned(), json!("snapshot"));
            source.insert(
                "sections".to_owned(),
                serde_json::to_value(sections).expect("section wire shape serializes"),
            );
        }
        Message::Known(Box::new(KnownMessage::User {
            id,
            content: vec![fr_wire::content::ContentBlock::Known(
                fr_wire::content::KnownContentBlock::Text {
                    text: text.to_owned(),
                },
            )],
            source: MessageSource::Other(source),
            extra: RawObject::new(),
        }))
    }

    fn input(in_history: bool, starts_series: bool) -> SystemPromptDecisionInput {
        SystemPromptDecisionInput {
            in_history,
            starts_series,
        }
    }

    /// Covers the event follow: an owned user message updates retention; a
    /// non-owned replacement citing the retained seq clears it; a
    /// replacement citing other seqs or a plain append leaves it alone.
    #[test]
    fn observe_follows_events() {
        use super::Retention;
        let replacement = |seq: u64, cited: u64| SessionEvent {
            seq: session_seq(seq),
            time: 0,
            data: SessionEventData::UserMessage(plain_user_message()),
            ignorable: None,
            surface_op: Some(SurfaceOp::Replace(ReplaceOp {
                op: ReplaceTag,
                start_seq: session_seq(cited),
                end_seq: session_seq(cited),
            })),
            source_event_seqs: Some(vec![session_seq(cited)]),
        };
        let mut projection = RuntimeContextProjection::restore(std::iter::empty(), &HashSet::new());
        let owned = owned_user_message(7, "ctx");
        projection.observe(&owned);
        assert_eq!(
            projection.retained,
            Retention::Held(RetainedSnapshot {
                seq: session_seq(7),
                text: Some("ctx".to_owned()),
            })
        );

        let citing = replacement(8, 7);
        projection.observe(&citing);
        assert_eq!(projection.retained, Retention::Cleared);

        // Re-retained, then a replacement citing another seq changes nothing.
        let owned = owned_user_message(9, "ctx2");
        projection.observe(&owned);
        let other = replacement(10, 3);
        projection.observe(&other);
        assert_eq!(
            projection.retained,
            Retention::Held(RetainedSnapshot {
                seq: session_seq(9),
                text: Some("ctx2".to_owned()),
            })
        );

        // A plain user message does not disturb retention.
        let plain = SessionEvent {
            seq: session_seq(11),
            time: 0,
            data: SessionEventData::UserMessage(plain_user_message()),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        projection.observe(&plain);
        assert_eq!(
            projection.retained,
            Retention::Held(RetainedSnapshot {
                seq: session_seq(9),
                text: Some("ctx2".to_owned()),
            })
        );
    }
}
