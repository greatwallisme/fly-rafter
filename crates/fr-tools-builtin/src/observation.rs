// Rust guideline compliant 2026-10-29
//! The filesystem observation gate (dsh `fs-observation-policy` port).
//!
//! The PRD 434 ruling places the observation policy in the fs tool
//! domain, so this is an in-crate module rather than a separate
//! plugin: the `fs/write-intent` and `fs/edit-intent` waterfalls are
//! dispatched by the tool bodies themselves (same crate, no
//! cross-plugin dispatch — the Q101′A dylib limitation) and the
//! `fs/observed` recorder is a synchronous direct call.
//!
//! The observed-file state keys first by the owner (the agent session
//! id; a call with no owner reads freely but cannot satisfy the
//! write/edit prior-observation policy), then by the opaque target
//! key. An entry's presence is the prior-observation record; its
//! discriminant keeps confirmed absence distinct from an unseen
//! target.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::fs_types::FsError;
use crate::fs_types::FsErrorCode;
use crate::fs_types::FsObservation;
use crate::fs_types::FsTarget;
use crate::fs_types::FsWriteIntent;

/// The observed-state owner (the dsh `FsObservationActor.agent.session`
/// identity, carried as the session id string).
type Owner = String;

/// Per-owner observed-file state and the three `fs/*` decisions over
/// it (the dsh `ObservedStateGate`).
#[derive(Debug, Default)]
pub struct ObservedStateGate {
    observed: Mutex<HashMap<Owner, HashMap<String, FsObservation>>>,
}

impl ObservedStateGate {
    /// Creates the empty gate (one instance per activation; the
    /// teardown drops all state).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all recorded state (the dsh disposal/HMR-safety arm).
    ///
    /// # Panics
    ///
    /// Poisoned lock (a panicking recorder held the mutex) — the
    /// recorder bodies cannot panic, so this is unreachable in
    /// practice.
    pub fn clear(&self) {
        self.observed
            .lock()
            .expect("the observed-state map cannot be poisoned by a panicking recorder")
            .clear();
    }

    /// Decide the write intent: unseen or confirmed absent ⇒
    /// `createIfAbsent`; confirmed present ⇒ `replaceIfVersion` at the
    /// observed version (the dsh `writeIntent`).
    #[must_use]
    pub fn write_intent(&self, target: &FsTarget, actor: Option<&str>) -> FsWriteIntent {
        let Some(owner) = actor else {
            return FsWriteIntent::CreateIfAbsent;
        };
        let prior = self.get(owner, &target.target_key);
        match prior {
            Some(FsObservation::Present { version }) => FsWriteIntent::ReplaceIfVersion { version },
            _ => FsWriteIntent::CreateIfAbsent,
        }
    }

    /// Decide the edit version guard: unseen rejects with
    /// `FS_NOT_OBSERVED`, confirmed absence rejects with
    /// `FS_NOT_FOUND`, and presence supplies the observed version as
    /// the CAS basis (the dsh `editIntent`).
    ///
    /// # Errors
    ///
    /// The two prior-observation refusals with their verbatim texts.
    pub fn edit_intent(&self, target: &FsTarget, actor: Option<&str>) -> Result<String, FsError> {
        let Some(owner) = actor else {
            return Err(FsError::new(
                FsErrorCode::NotObserved,
                format!("edit requires reading \"{}\" first", target.display_path),
            ));
        };
        let prior = self.get(owner, &target.target_key);
        match prior {
            None => Err(FsError::new(
                FsErrorCode::NotObserved,
                format!("edit requires reading \"{}\" first", target.display_path),
            )),
            Some(FsObservation::Absent) => Err(FsError::new(
                FsErrorCode::NotFound,
                format!("cannot edit \"{}\": not found", target.display_path),
            )),
            Some(FsObservation::Present { version }) => Ok(version),
        }
    }

    /// Record an authoritative present or absent observation for this
    /// owner and target (the dsh `observe`; a no-owner call records
    /// nothing useful, mirroring the source).
    ///
    /// # Panics
    ///
    /// Poisoned lock (a panicking recorder held the mutex) — the
    /// recorder bodies cannot panic, so this is unreachable in
    /// practice.
    pub fn observe(&self, target: &FsTarget, observation: FsObservation, actor: Option<&str>) {
        let Some(owner) = actor else {
            return;
        };
        self.observed
            .lock()
            .expect("the observed-state map cannot be poisoned by a panicking recorder")
            .entry(owner.to_owned())
            .or_default()
            .insert(target.target_key.clone(), observation);
    }

    fn get(&self, owner: &str, target_key: &str) -> Option<FsObservation> {
        self.observed
            .lock()
            .expect("the observed-state map cannot be poisoned by a panicking recorder")
            .get(owner)
            .and_then(|by_target| by_target.get(target_key))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(key: &str) -> FsTarget {
        FsTarget {
            target_key: key.to_owned(),
            display_path: format!("/{key}"),
        }
    }

    #[test]
    fn write_intent_defaults_to_create_when_unseen() {
        let gate = ObservedStateGate::new();
        assert_eq!(
            gate.write_intent(&target("a"), Some("s1")),
            FsWriteIntent::CreateIfAbsent
        );
        assert_eq!(
            gate.write_intent(&target("a"), None),
            FsWriteIntent::CreateIfAbsent
        );
    }

    #[test]
    fn present_observation_guards_replace() {
        let gate = ObservedStateGate::new();
        gate.observe(
            &target("a"),
            FsObservation::Present {
                version: "v1".into(),
            },
            Some("s1"),
        );
        assert_eq!(
            gate.write_intent(&target("a"), Some("s1")),
            FsWriteIntent::ReplaceIfVersion {
                version: "v1".into()
            }
        );
    }

    #[test]
    fn edit_requires_prior_read() {
        let gate = ObservedStateGate::new();
        let error = gate.edit_intent(&target("a"), Some("s1")).unwrap_err();
        assert_eq!(error.code, FsErrorCode::NotObserved);
        assert_eq!(error.message, "edit requires reading \"/a\" first");
        let error = gate.edit_intent(&target("a"), None).unwrap_err();
        assert_eq!(error.code, FsErrorCode::NotObserved);
    }

    #[test]
    fn absent_observation_rejects_edit_not_found() {
        let gate = ObservedStateGate::new();
        gate.observe(&target("a"), FsObservation::Absent, Some("s1"));
        let error = gate.edit_intent(&target("a"), Some("s1")).unwrap_err();
        assert_eq!(error.code, FsErrorCode::NotFound);
        assert_eq!(error.message, "cannot edit \"/a\": not found");
    }

    #[test]
    fn observation_is_owner_scoped_and_clearable() {
        let gate = ObservedStateGate::new();
        gate.observe(
            &target("a"),
            FsObservation::Present {
                version: "v1".into(),
            },
            Some("s1"),
        );
        assert_eq!(
            gate.write_intent(&target("a"), Some("s2")),
            FsWriteIntent::CreateIfAbsent
        );
        gate.clear();
        assert_eq!(
            gate.write_intent(&target("a"), Some("s1")),
            FsWriteIntent::CreateIfAbsent
        );
    }
}
