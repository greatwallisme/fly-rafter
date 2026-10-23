//! The session event vocabulary: core types plus plugin declarations
//! aggregated at activation time (PRD 7.1, 2026-10-23 Q34A ruling), and the
//! read-path refusal of unknown required events.

use std::collections::HashSet;

use crate::error::SessionError;
use fr_wire::event::event_type;

/// The aggregated set of event type names this installation understands.
///
/// The set is the fr-session core types plus every plugin's activation-time
/// declaration. Duplicate declaration is an assembly error (fail-closed,
/// mirroring the compiler uniqueness dsh gets from TypeScript).
#[derive(Debug, Clone, Default)]
pub struct EventVocabulary {
    known: HashSet<String>,
}

impl EventVocabulary {
    /// The vocabulary holding exactly the fr-session core event types.
    #[must_use]
    pub fn core() -> Self {
        Self {
            known: [
                event_type::TURN_START,
                event_type::TURN_END,
                event_type::STEP_START,
                event_type::STEP_END,
                event_type::USER_MESSAGE,
                event_type::DEVELOPER_MESSAGE,
                event_type::SYSTEM_MESSAGE,
                event_type::ASSISTANT_MESSAGE,
                event_type::ASSISTANT_ATTEMPT,
                event_type::TOOL_CALL,
                event_type::TOOL_RESULT,
                event_type::REQUEST_HEADER,
                event_type::REQUEST_CONTEXT,
                event_type::SESSION_END_SEED,
            ]
            .iter()
            .map(ToString::to_string)
            .collect(),
        }
    }

    /// Declares plugin-owned durable event type names at activation time.
    ///
    /// # Errors
    ///
    /// Returns an error naming the type when it is already known (declared
    /// twice by one plugin, or claimed by another plugin or the core).
    pub fn declare(&mut self, types: &[String]) -> Result<(), SessionError> {
        for name in types {
            if !self.known.insert(name.clone()) {
                return Err(SessionError::Other(format!(
                    "session event type \"{name}\" is already declared"
                )));
            }
        }
        Ok(())
    }

    /// Whether the type is understood by this installation.
    #[must_use]
    pub fn knows(&self, name: &str) -> bool {
        self.known.contains(name)
    }
}

/// The read-path vocabulary gate: a log containing an event type outside
/// the known set without the `ignorable` marker MUST refuse reconstruction
/// (PRD 7.1; dsh validation.ts message verbatim).
///
/// # Errors
///
/// Returns the dsh refusal message for the first unknown required event.
pub fn refuse_unknown_required(
    events: &[fr_wire::event::SessionEvent],
    vocabulary: &EventVocabulary,
) -> Result<(), SessionError> {
    for event in events {
        if vocabulary.knows(event.event_type()) || event.ignorable.is_some() {
            continue;
        }
        return Err(SessionError::Other(format!(
            "format v4 contains unknown event type {} at seq {}",
            serde_json::to_string(event.event_type()).unwrap_or_default(),
            event.seq.get()
        )));
    }
    Ok(())
}
