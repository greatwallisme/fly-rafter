// Rust guideline compliant 2026-11-04
//! Adoption checks for `--session-id` runs (the dsh headless `index.ts`
//! `currentPreset`/`assertAdoptable` faces, wording verbatim per the Q2A
//! verbatim ruling).
//!
//! The checks run over the session's creation header and durable events:
//! the preset a session currently runs under is its header value advanced
//! by the last `agent-preset/selected` record, and a corrupt record must
//! fail loud rather than read as "no preset" (the run would otherwise
//! continue under this bundle's composition instead of the recorded one).

use fr_wire::event::{SessionEvent, SessionEventData, SessionHeader};

/// The preset a session currently runs under: its creation header advanced
/// by the last `agent-preset/selected` record in `events`.
///
/// The record type is owned by the presets plugin, which the headless
/// bundle does not compose, so it is read structurally (dsh reads the
/// candidate's shape rather than through its module augmentation).
///
/// # Errors
///
/// A malformed record (a non-string or empty selection) must not read as
/// "no preset": the error names the session so the operator sees the
/// corruption instead of a silent composition switch.
pub fn current_preset(
    header: &SessionHeader,
    events: &[SessionEvent],
    session_id: &str,
) -> Result<Option<String>, String> {
    let mut preset = header.agent_preset.clone();
    for event in events {
        let SessionEventData::Unknown(unknown) = &event.data else {
            continue;
        };
        if unknown.type_name != "agent-preset/selected" {
            continue;
        }
        let selected = unknown.data.get("agentPreset");
        match selected.and_then(serde_json::Value::as_str) {
            Some(text) if !text.is_empty() => preset = Some(text.to_owned()),
            _ => {
                return Err(format!(
                    "session \"{session_id}\" records a malformed agent-preset/selected event and cannot be adopted"
                ));
            }
        }
    }
    Ok(preset)
}

/// The adoption refusals and the runner's service double-checks, wording
/// verbatim (dsh `index.ts` `assertAdoptable`/`resolveAgent`).
pub mod refusals {
    /// The session runs under an agent preset the one-shot runner does not
    /// compose.
    #[must_use]
    pub fn preset_mismatch(session_id: &str, preset: &str) -> String {
        format!(
            "session \"{session_id}\" runs under agent preset \"{preset}\", \
             which the one-shot runner does not compose"
        )
    }

    /// The session is a subagent or fork.
    #[must_use]
    pub fn subagent_or_fork(session_id: &str) -> String {
        format!(
            "session \"{session_id}\" is a subagent or forked session and \
             cannot be driven directly"
        )
    }

    /// The session recorded no working directory.
    #[must_use]
    pub fn missing_cwd(session_id: &str) -> String {
        format!(
            "session \"{session_id}\" recorded no working directory, so it \
             cannot be adopted"
        )
    }

    /// The recorded working directory differs from this run's.
    #[must_use]
    pub fn cwd_mismatch(session_id: &str, recorded: &str, current: &str) -> String {
        format!("session \"{session_id}\" was recorded in \"{recorded}\", not \"{current}\"")
    }

    /// A live agent already owns the session.
    #[must_use]
    pub fn live_agent(session_id: &str) -> String {
        format!(
            "session \"{session_id}\" is live in this process, so the \
             one-shot runner cannot own an exclusive run interval"
        )
    }

    /// The session does not exist.
    #[must_use]
    pub fn not_found(session_id: &str) -> String {
        format!(
            "session \"{session_id}\" does not exist; omit --session-id to \
             start a new Session"
        )
    }

    /// The persistence service is missing.
    pub const PERSISTENCE_REQUIRED: &str = "headless --session-id requires the sessionPersistence service; the Session would not survive this process";

    /// The query service is missing.
    pub const QUERY_REQUIRED: &str =
        "headless --session-id requires the sessionQuery service; dsh-base provides it";
}

/// Rejects a session the one-shot runner must not adopt, in the dsh order:
/// preset first (a real mismatch is named instead of the generic live
/// refusal), then subagent/fork lineage, then the cwd facts.
///
/// # Errors
///
/// Returns the verbatim refusal text for the first failed rule.
pub fn assert_adoptable(
    header: &SessionHeader,
    events: &[SessionEvent],
    session_id: &str,
    cwd: &str,
) -> Result<(), String> {
    if let Some(preset) = current_preset(header, events, session_id)? {
        return Err(refusals::preset_mismatch(session_id, &preset));
    }
    if header.origin.is_some() || header.parent_session.is_some() {
        return Err(refusals::subagent_or_fork(session_id));
    }
    let Some(recorded) = header.cwd.as_deref() else {
        return Err(refusals::missing_cwd(session_id));
    };
    if recorded != cwd {
        return Err(refusals::cwd_mismatch(session_id, recorded, cwd));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{assert_adoptable, current_preset, refusals};
    use fr_wire::event::{SessionEvent, SessionEventData, SessionHeader};
    use fr_wire::ids::SessionSeq;

    /// One durable event carrying an unknown-type payload (the presets
    /// plugin's record type, read structurally).
    fn preset_event(selection: serde_json::Value) -> SessionEvent {
        let data = SessionEventData::from_parts("agent-preset/selected", selection)
            .expect("unknown record decodes verbatim");
        SessionEvent {
            seq: SessionSeq::new(2).expect("in range"),
            time: 0,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    /// A plain headless creation header: top-level, cwd recorded, no preset.
    fn header() -> SessionHeader {
        SessionHeader {
            version: 4,
            id: fr_wire::SessionId::new("session-x"),
            created_at: 0,
            cwd: Some("/repo".into()),
            parent_session: None,
            is_seeded: false,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    #[test]
    fn header_without_records_carries_the_preset() {
        let header = header();
        assert_eq!(
            current_preset(&header, &[], "session-x").expect("preset"),
            None
        );
        assert_adoptable(&header, &[], "session-x", "/repo").expect("adoptable");
    }

    #[test]
    fn selected_record_advances_the_preset() {
        let header = header();
        let events = [preset_event(serde_json::json!({"agentPreset": "code"}))];
        let preset = current_preset(&header, &events, "session-x").expect("preset");
        assert_eq!(preset.as_deref(), Some("code"));
        let error =
            assert_adoptable(&header, &events, "session-x", "/repo").expect_err("preset mismatch");
        assert_eq!(error, refusals::preset_mismatch("session-x", "code"));
    }

    #[test]
    fn corrupt_record_fails_instead_of_reading_as_no_preset() {
        let header = header();
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"agentPreset": ""}),
        ] {
            let events = [preset_event(bad)];
            let error = current_preset(&header, &events, "session-x").expect_err("corrupt");
            assert!(error.contains("records a malformed agent-preset/selected event"));
        }
    }

    #[test]
    fn subagent_and_fork_lineages_are_refused() {
        let mut subagent = header();
        subagent.origin = Some(fr_wire::event::Origin::Subagent);
        let error = assert_adoptable(&subagent, &[], "session-x", "/repo").expect_err("subagent");
        assert_eq!(error, refusals::subagent_or_fork("session-x"));

        let mut forked = header();
        forked.parent_session = Some(fr_wire::SessionId::new("session-root"));
        let error = assert_adoptable(&forked, &[], "session-x", "/repo").expect_err("fork");
        assert_eq!(error, refusals::subagent_or_fork("session-x"));
    }

    #[test]
    fn cwd_facts_are_enforced_in_order() {
        let mut no_cwd = header();
        no_cwd.cwd = None;
        let error = assert_adoptable(&no_cwd, &[], "session-x", "/repo").expect_err("no cwd");
        assert_eq!(error, refusals::missing_cwd("session-x"));

        let error =
            assert_adoptable(&header(), &[], "session-x", "/elsewhere").expect_err("cwd mismatch");
        assert_eq!(
            error,
            refusals::cwd_mismatch("session-x", "/repo", "/elsewhere")
        );
    }

    #[test]
    fn refusal_texts_are_verbatim() {
        assert_eq!(
            refusals::live_agent("s1"),
            "session \"s1\" is live in this process, so the one-shot runner cannot own an exclusive run interval"
        );
        assert_eq!(
            refusals::not_found("s1"),
            "session \"s1\" does not exist; omit --session-id to start a new Session"
        );
        assert_eq!(
            refusals::PERSISTENCE_REQUIRED,
            "headless --session-id requires the sessionPersistence service; the Session would not survive this process"
        );
        assert_eq!(
            refusals::QUERY_REQUIRED,
            "headless --session-id requires the sessionQuery service; dsh-base provides it"
        );
    }
}
