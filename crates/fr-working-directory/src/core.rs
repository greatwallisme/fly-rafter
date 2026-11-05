// Rust guideline compliant 2026-11-05
//! The session working directories (dsh `working-directory` 1:1; PRD
//! decision 47-3A: an independent plugin owning the committed directory
//! and its model-visible changes).
//!
//! The full arm: per-session serialized `ensure`/`set`, the committed
//! change records with their model notices, and the runtime-context
//! text.
//!
//! The directory legs ride the [`DirectoryLeg`] seam (the dsh
//! `ctx.fs.resolve/stat/processPath` isomorph; the plugin shell
//! implements it over the `'fs'` service); the durable changes ride the
//! `'session-persistence'` faces; the notices ride the `'agent'` inject
//! face.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;

use crate::wire::CHANGE_EVENT;

/// The projection key (dsh `workingDirectory`).
pub const PROJECTION_KEY: &str = "workingDirectory";
/// The projection format version (dsh `stateVersion: 1`).
pub const STATE_VERSION: u64 = 1;

/// The session-log seam: cold-reads one session's header plus the
/// event window the self-read folds, and appends the durable change
/// record.
pub trait SessionLog: Send + Sync {
    /// Reads the stored log (the dsh `read` reply shape: header +
    /// inherited prefix + full events).
    ///
    /// # Errors
    ///
    /// The persistence refusal.
    fn read(&self, id: &str) -> Result<ColdLog, String>;

    /// Appends the durable change record (`working-directory/change`).
    ///
    /// # Errors
    ///
    /// The persistence refusal.
    fn append(&self, id: &str, cwd: &str) -> Result<(), String>;
}

/// The agent-inject seam (the dsh `agent.inject(notice)`): queues one
/// message into the agent's step queue.
pub trait AgentInject: Send + Sync {
    /// Queues the notice message.
    ///
    /// # Errors
    ///
    /// The transport refusal.
    fn inject(&self, agent: &str, message: &Value) -> Result<(), String>;
}

/// The directory seam (the dsh `ctx.fs.resolve`/`stat`/`processPath`
/// isomorph).
///
/// The target token is opaque: the producer of [`Self`] defines its
/// spelling (the wire carrier carries the fs target JSON; tests carry
/// plain paths).
pub trait DirectoryLeg: Send + Sync {
    /// Resolves `path` against `cwd` into the opaque target token
    /// (pure path algebra; existence is [`Self::stat_is_directory`]'s
    /// question).
    ///
    /// # Errors
    ///
    /// The resolution refusal message.
    fn resolve(&self, path: &str, cwd: Option<&str>) -> Result<String, String>;

    /// Inspects the target: `Some(true)` when it exists and is a
    /// directory, `Some(false)` when it exists as something else, and
    /// `None` when it is missing.
    ///
    /// # Errors
    ///
    /// The inspection failure message.
    fn stat_is_directory(&self, target: &str) -> Result<Option<bool>, String>;

    /// Spells the canonical absolute path a subprocess in this
    /// execution world can open (the dsh `processPath`).
    fn process_path(&self, target: &str) -> String;
}

/// The cold log the fold kernel projects (the dsh `read` reply shape:
/// header + inherited prefix + full events).
#[derive(Debug, Clone)]
pub struct ColdLog {
    /// The logical session header.
    pub header: Value,
    /// The exact inherited prefix length.
    pub inherited_event_count: u64,
    /// The preserved events in log order (the JSON array form).
    pub events: Value,
}

/// The projection state (dsh `workingDirectory` state:
/// `header.cwd ?? null` advanced by change events).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionState {
    /// No committed directory.
    Null,
    /// The committed absolute directory.
    At(String),
}

/// Projects the cold log onto the committed directory (the dsh fold:
/// `init: header.cwd ?? null`, `apply: change events`).
#[must_use]
pub fn fold(log: &ColdLog) -> ProjectionState {
    let mut state = log
        .header
        .get("cwd")
        .and_then(Value::as_str)
        .map_or(ProjectionState::Null, |cwd| {
            ProjectionState::At(cwd.to_owned())
        });
    if let Value::Array(items) = &log.events {
        for event in items {
            if event.get("type") == Some(&Value::String(CHANGE_EVENT.to_owned()))
                && let Some(cwd) = event.pointer("/data/cwd").and_then(Value::as_str)
            {
                state = ProjectionState::At(cwd.to_owned());
            }
        }
    }
    state
}
/// The working-directory domain (dsh `working-directory` full arm):
/// per-session serialized `ensure`/`set`, the committed read, the
/// model notices, and the runtime-context text.
pub struct WorkingDirectories {
    /// One serialization gate per session (dsh `serialize`).
    gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    log: Arc<dyn SessionLog>,
    leg: Arc<dyn DirectoryLeg>,
    inject: Arc<dyn AgentInject>,
    default_dir: String,
}

impl std::fmt::Debug for WorkingDirectories {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkingDirectories")
            .field("default_dir", &self.default_dir)
            .finish_non_exhaustive()
    }
}

impl WorkingDirectories {
    /// Assembles the domain over the log/inject seams, the directory
    /// leg, and the launch directory (dsh `defaultDirectory`).
    #[must_use]
    pub fn new(
        log: Arc<dyn SessionLog>,
        leg: Arc<dyn DirectoryLeg>,
        inject: Arc<dyn AgentInject>,
        default_dir: String,
    ) -> Self {
        Self {
            gates: Mutex::new(HashMap::new()),
            log,
            leg,
            inject,
            default_dir,
        }
    }

    /// Reads the committed directory without filesystem I/O (dsh
    /// `get`: the projection state, else the default).
    ///
    /// # Errors
    ///
    /// The log read failure.
    ///
    /// # Panics
    ///
    /// Panics when the gate-table or session-gate lock is poisoned.
    pub fn get(&self, agent: &str) -> Result<String, String> {
        Ok(match self.fold_self(agent)? {
            ProjectionState::At(cwd) => cwd,
            ProjectionState::Null => self.default_dir.clone(),
        })
    }

    /// Validates the current directory and restores the original
    /// project when it disappeared (dsh `ensure` full arm): serialized
    /// per session, the existing directory returns as-is (committing a
    /// change record when the projection was null), a vanished
    /// directory restores `header.cwd ?? default` with the recovered
    /// commit, and an also-vanished original refuses.
    ///
    /// # Errors
    ///
    /// The log read or append failure, a failing inspection, or the
    /// dsh recovery refusal when the original is unavailable too.
    ///
    /// # Panics
    ///
    /// Panics when the gate-table or session-gate lock is poisoned.
    pub fn ensure(&self, agent: &str) -> Result<String, String> {
        let gate = Arc::clone(
            self.gates
                .lock()
                .expect("gate table")
                .entry(agent.to_owned())
                .or_default(),
        );
        let _serialized = gate.lock().expect("session gate");
        let log = self.log.read(agent)?;
        let original = log
            .header
            .get("cwd")
            .and_then(Value::as_str)
            .map_or_else(|| self.default_dir.clone(), str::to_owned);
        let state = fold(&log);
        let current = match &state {
            ProjectionState::At(cwd) => cwd.clone(),
            ProjectionState::Null => self.default_dir.clone(),
        };
        if self.directory_exists(current.as_str())? {
            // The dsh commit arm: a null projection is backfilled with
            // a change record so third-party relays converge.
            if state == ProjectionState::Null {
                self.log.append(agent, &current)?;
            }
            return Ok(current);
        }
        // The dsh recovery leg: the restore target is the header's own
        // project, resolved against itself and inspected through the
        // same directory leg.
        let restored = self.directory(&original, &original)?;
        self.commit(agent, &restored, true)?;
        Ok(restored)
    }

    /// Changes one session's directory without changing existing
    /// processes or permissions (dsh `set`): the empty refusal, the
    /// directory leg resolved against the current directory, and the
    /// changed commit.
    ///
    /// # Errors
    ///
    /// The empty refusal, the dsh not-a-directory refusal, or the log
    /// read/append failure.
    ///
    /// # Panics
    ///
    /// Panics when the gate-table or session-gate lock is poisoned.
    pub fn set(&self, agent: &str, path: &str) -> Result<String, String> {
        let gate = Arc::clone(
            self.gates
                .lock()
                .expect("gate table")
                .entry(agent.to_owned())
                .or_default(),
        );
        let _serialized = gate.lock().expect("session gate");
        if path.is_empty() {
            return Err("working-directory: cd must not be empty".to_owned());
        }
        let current = self.get(agent)?;
        let directory = self.directory(path, &current)?;
        self.commit(agent, &directory, false)?;
        Ok(directory)
    }

    /// The runtime-context text for one agent (the dsh
    /// `systemPrompt.context` text plus its assemble prepend listener
    /// reduced to the single resolve: the ensure leg runs first, then
    /// the text reflects the ensured directory).
    ///
    /// # Errors
    ///
    /// The ensure or log-read failure.
    ///
    /// # Panics
    ///
    /// Panics when the gate-table or session-gate lock is poisoned.
    pub fn context_text(&self, agent: Option<&str>) -> Result<String, String> {
        let Some(agent) = agent else {
            return Ok(String::new());
        };
        self.ensure(agent)?;
        let cwd = self.get(agent)?;
        Ok(format!("Current working directory: {}.", quote(&cwd)))
    }

    /// The existence probe behind the dsh `fs.resolve+stat` pair for
    /// the ensure's current-directory leg (absolute input, no cwd).
    fn directory_exists(&self, path: &str) -> Result<bool, String> {
        let target = self.leg.resolve(path, None)?;
        Ok(self.leg.stat_is_directory(&target)?.unwrap_or(false))
    }

    /// The dsh `directory` helper: resolves `path` against `cwd`,
    /// refuses anything that is not an existing directory (the text
    /// carries the raw input spelling), and returns the canonical
    /// absolute path.
    fn directory(&self, path: &str, cwd: &str) -> Result<String, String> {
        let target = self.leg.resolve(path, Some(cwd))?;
        let is_dir = self.leg.stat_is_directory(&target)?.unwrap_or(false);
        if !is_dir {
            return Err(format!(
                "working-directory: directory does not exist: {}",
                quote(path)
            ));
        }
        Ok(self.leg.process_path(&target))
    }

    /// The dsh `commit`: no-op when the directory did not move, the
    /// durable change append, and the model notice with its
    /// recovered/changed variant. A notice refusal never reverts the
    /// commit (the dsh warn-not-rollback judgment; the fr carrier has
    /// no logger face, so the refusal stays contained).
    fn commit(&self, agent: &str, directory: &str, recovered: bool) -> Result<(), String> {
        let previous = self.get(agent)?;
        if previous == directory {
            return Ok(());
        }
        self.log.append(agent, directory)?;
        let notice = notice_message(&previous, directory, recovered);
        // The dsh warn-not-rollback arm: the notice refusal is
        // contained and the committed state stands.
        let _ = self.inject.inject(agent, &notice);
        Ok(())
    }

    /// Pulls the log tail and folds it locally (the Q110A-prime
    /// self-read shape).
    fn fold_self(&self, agent: &str) -> Result<ProjectionState, String> {
        Ok(fold(&self.log.read(agent)?))
    }
}

/// Builds the model notice (the dsh `createUserMessage` pair).
///
/// The recovered/changed variants carry one text block, a fresh UUID
/// v4 identity, and the `working-directory` source (the `job_tools`
/// notice judgment).
#[must_use]
pub fn notice_message(previous: &str, directory: &str, recovered: bool) -> Value {
    let text = if recovered {
        format!(
            "The working directory {} is unavailable. The working directory is now {}.",
            quote(previous),
            quote(directory)
        )
    } else {
        format!(
            "The working directory changed from {} to {}.",
            quote(previous),
            quote(directory)
        )
    };
    serde_json::json!({
        "role": "user",
        "id": uuid::Uuid::new_v4().to_string(),
        "content": [{ "type": "text", "text": text }],
        "source": { "kind": "working-directory" },
    })
}

/// The dsh `JSON.stringify` quoting for refusal and notice texts.
#[must_use]
fn quote(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared test seams: one std-fs directory leg plus the fake
    //! log/inject pair (the in-process fakes the fr-tools-builtin arm
    //! carried; the leg stands in for the wire carrier).

    use super::{AgentInject, CHANGE_EVENT, ColdLog, DirectoryLeg, SessionLog};
    use serde_json::Value;
    use std::path::Path;
    use std::sync::Mutex;

    /// The std-fs directory leg (the dsh `resolveLocalTarget` algebra
    /// for the test shapes: absolute inputs pass through, relative
    /// inputs join the cwd; existence is a plain metadata probe).
    #[derive(Debug, Default)]
    pub struct StdLeg;

    impl DirectoryLeg for StdLeg {
        fn resolve(&self, path: &str, cwd: Option<&str>) -> Result<String, String> {
            if Path::new(path).is_absolute() {
                return Ok(path.to_owned());
            }
            let Some(cwd) = cwd else {
                return Err("relative path without a cwd".to_owned());
            };
            Ok(Path::new(cwd).join(path).to_string_lossy().into_owned())
        }

        fn stat_is_directory(&self, target: &str) -> Result<Option<bool>, String> {
            Ok(std::fs::metadata(target)
                .ok()
                .map(|metadata| metadata.is_dir()))
        }

        fn process_path(&self, target: &str) -> String {
            std::fs::canonicalize(target).map_or_else(
                |_| target.to_owned(),
                |path| path.to_string_lossy().into_owned(),
            )
        }
    }

    /// One fake log: a fixed header and an appendable change list.
    #[derive(Debug, Default)]
    pub struct FakeLog {
        header: Mutex<Option<Value>>,
        seed: Mutex<Vec<String>>,
        changes: Mutex<Vec<String>>,
    }

    impl FakeLog {
        #[must_use]
        pub fn with_header(cwd: &str) -> Self {
            Self {
                header: Mutex::new(Some(serde_json::json!({ "cwd": cwd }))),
                ..Self::default()
            }
        }

        #[must_use]
        pub fn with_changes(paths: &[&str]) -> Self {
            Self {
                seed: Mutex::new(paths.iter().map(|path| (*path).to_owned()).collect()),
                ..Self::default()
            }
        }

        /// The committed change count (the append assertions).
        ///
        /// # Panics
        ///
        /// Panics when the change lock is poisoned.
        pub fn change_count(&self) -> usize {
            self.changes.lock().expect("changes").len()
        }
    }

    impl SessionLog for FakeLog {
        fn read(&self, _id: &str) -> Result<ColdLog, String> {
            let header = self
                .header
                .lock()
                .expect("header")
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));
            let mut changes = self.changes.lock().expect("changes").clone();
            changes.extend(self.seed.lock().expect("seed").iter().cloned());
            let events = Value::Array(
                changes
                    .into_iter()
                    .map(|cwd| serde_json::json!({ "type": CHANGE_EVENT, "data": { "cwd": cwd } }))
                    .collect(),
            );
            Ok(ColdLog {
                header,
                inherited_event_count: 0,
                events,
            })
        }

        fn append(&self, _id: &str, cwd: &str) -> Result<(), String> {
            self.changes.lock().expect("changes").push(cwd.to_owned());
            Ok(())
        }
    }

    /// One fake inject: records the queued notice messages and can be
    /// armed to refuse.
    #[derive(Debug, Default)]
    pub struct FakeInject {
        refused: std::sync::atomic::AtomicBool,
        notices: Mutex<Vec<String>>,
    }

    impl FakeInject {
        /// Arms the transport refusal.
        #[must_use]
        pub fn refusing() -> Self {
            Self {
                refused: std::sync::atomic::AtomicBool::new(true),
                notices: Mutex::new(Vec::new()),
            }
        }

        /// The recorded notice texts (the variant assertions).
        ///
        /// # Panics
        ///
        /// Panics when the notice lock is poisoned.
        pub fn notices(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
            self.notices.lock().expect("notices")
        }
    }

    impl AgentInject for FakeInject {
        fn inject(&self, _agent: &str, message: &Value) -> Result<(), String> {
            if self.refused.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("inject transport down".to_owned());
            }
            let text = message
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            self.notices.lock().expect("notices").push(text);
            Ok(())
        }
    }

    /// One unique scratch root per test (cleaned on drop); the
    /// scripted directory set becomes real directories under it.
    pub struct Scratch(std::path::PathBuf);

    impl Scratch {
        #[must_use]
        pub fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-wd-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir creates");
            Self(dir)
        }

        #[must_use]
        pub fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }

        #[must_use]
        pub fn make(&self, name: &str) -> String {
            std::fs::create_dir_all(self.0.join(name)).expect("scratch subdir creates");
            self.path(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::{FakeInject, FakeLog, Scratch, StdLeg};

    /// Assembles the domain over a fake log/inject pair and the std-fs
    /// leg rooted at the scratch.
    fn domain(
        log: FakeLog,
        inject: FakeInject,
        existing: &[&str],
    ) -> (WorkingDirectories, Arc<FakeLog>, Arc<FakeInject>, Scratch) {
        let scratch = Scratch::new("domain");
        for name in existing {
            let _ = scratch.make(name);
        }
        let log: Arc<FakeLog> = Arc::new(log);
        let inject: Arc<FakeInject> = Arc::new(inject);
        let wd = WorkingDirectories::new(
            Arc::clone(&log) as Arc<dyn SessionLog>,
            Arc::new(StdLeg),
            Arc::clone(&inject) as Arc<dyn AgentInject>,
            scratch.path("launch"),
        );
        (wd, log, inject, scratch)
    }

    /// The fold kernel: header init, then change events in log order.
    #[test]
    fn fold_inits_from_header_and_applies_changes() {
        let log = ColdLog {
            header: serde_json::json!({ "cwd": "/repo" }),
            inherited_event_count: 0,
            events: Value::Array(vec![
                serde_json::json!({ "type": "other/event", "data": { "cwd": "/x" } }),
                serde_json::json!({ "type": CHANGE_EVENT, "data": { "cwd": "/b" } }),
                serde_json::json!({ "type": CHANGE_EVENT, "data": { "cwd": "/c" } }),
            ]),
        };
        assert_eq!(fold(&log), ProjectionState::At("/c".to_owned()));
        let bare = ColdLog {
            header: serde_json::json!({}),
            inherited_event_count: 0,
            events: Value::Array(Vec::new()),
        };
        assert_eq!(fold(&bare), ProjectionState::Null);
    }

    /// An existing directory returns as-is; a null projection is
    /// backfilled with one change record.
    #[test]
    fn ensure_returns_existing_and_backfills_null_projection() {
        let (wd, log, _inject, scratch) =
            domain(FakeLog::default(), FakeInject::default(), &["launch"]);
        let got = wd.ensure("s1").expect("ensure");
        assert_eq!(got, scratch.path("launch"));
        // The null projection was backfilled exactly once.
        assert_eq!(log.change_count(), 1);
        let second = wd.ensure("s1").expect("second ensure");
        assert_eq!(second, scratch.path("launch"));
        assert_eq!(log.change_count(), 1);
    }

    /// A vanished committed directory restores the original with the
    /// recovered commit; an also-vanished original refuses with the
    /// dsh text.
    #[test]
    fn ensure_restores_the_original_and_refuses_when_it_is_gone_too() {
        // A committed /gone (the change log), no header cwd, default
        // launch exists: restore + recovered commit + recovered notice.
        let (wd, log, inject, scratch) = domain(
            FakeLog::with_changes(&["/gone"]),
            FakeInject::default(),
            &["launch"],
        );
        assert_eq!(wd.ensure("s1").expect("ensure"), scratch.path("launch"));
        assert_eq!(log.change_count(), 1);
        assert_eq!(inject.notices().len(), 1);
        assert!(inject.notices()[0].contains(&format!(
            "is unavailable. The working directory is now {}.",
            quote(&scratch.path("launch"))
        )));
        // A vanished header cwd is also-unavailable: the dsh recovery
        // refusal (the header's own project is the restore target; the
        // default launch dir exists but the committed state is the
        // vanished header project).
        let (wd, _log, _inject, _scratch) = domain(
            FakeLog::with_header("/nonexistent-repo"),
            FakeInject::default(),
            &["launch"],
        );
        assert_eq!(
            wd.ensure("s1").unwrap_err(),
            "working-directory: directory does not exist: \"/nonexistent-repo\""
        );
        // Neither exists: the dsh recovery refusal.
        let (wd, _log, _inject, _scratch) = domain(
            FakeLog::with_header("/nonexistent-repo-2"),
            FakeInject::default(),
            &[],
        );
        assert_eq!(
            wd.ensure("s1").unwrap_err(),
            "working-directory: directory does not exist: \"/nonexistent-repo-2\""
        );
    }

    /// `set` resolves against the current directory, commits once, and
    /// queues the changed-variant notice.
    #[test]
    fn set_commits_and_notifies_the_changed_variant() {
        let (wd, log, inject, scratch) = domain(
            FakeLog::default(),
            FakeInject::default(),
            &["launch", "target"],
        );
        let got = wd.set("s1", &scratch.path("target")).expect("set");
        assert_eq!(got, scratch.path("target"));
        assert_eq!(log.change_count(), 1);
        let notices = inject.notices();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0],
            format!(
                "The working directory changed from {} to {}.",
                quote(&scratch.path("launch")),
                quote(&scratch.path("target"))
            )
        );
    }

    /// The empty and not-a-directory refusals carry the dsh texts and
    /// commit nothing.
    #[test]
    fn set_refuses_empty_and_nonexistent_without_committing() {
        let (wd, log, inject, _scratch) =
            domain(FakeLog::default(), FakeInject::default(), &["launch"]);
        assert_eq!(
            wd.set("s1", "").unwrap_err(),
            "working-directory: cd must not be empty"
        );
        assert_eq!(
            wd.set("s1", "/nonexistent-target-dir").unwrap_err(),
            "working-directory: directory does not exist: \"/nonexistent-target-dir\""
        );
        assert_eq!(log.change_count(), 0);
        assert_eq!(inject.notices().len(), 0);
    }

    /// Setting the directory that is already current short-circuits:
    /// no append and no notice, still resolving the canonical form.
    #[test]
    fn set_same_directory_short_circuits() {
        let (wd, log, inject, scratch) = domain(
            FakeLog::default(),
            FakeInject::default(),
            &["launch", "target"],
        );
        wd.set("s1", &scratch.path("target")).expect("first set");
        wd.set("s1", &scratch.path("target")).expect("second set");
        assert_eq!(log.change_count(), 1);
        assert_eq!(inject.notices().len(), 1);
    }

    /// A notice refusal is contained: the commit stands (the dsh
    /// warn-not-rollback judgment).
    #[test]
    fn set_commits_when_the_notice_refuses() {
        let (wd, log, _inject, scratch) = domain(
            FakeLog::default(),
            FakeInject::refusing(),
            &["launch", "target"],
        );
        let got = wd.set("s1", &scratch.path("target")).expect("set");
        assert_eq!(got, scratch.path("target"));
        assert_eq!(log.change_count(), 1);
    }

    /// The context text is empty without an agent and reflects the
    /// ensured directory with one (the PROMPT contract's
    /// working-directory context clause).
    #[test]
    fn context_tracks_the_working_directory() {
        let (wd, _log, _inject, scratch) = domain(
            FakeLog::default(),
            FakeInject::default(),
            &["launch", "target"],
        );
        assert_eq!(wd.context_text(None).expect("empty context"), "");
        let first = wd.context_text(Some("s1")).expect("context");
        assert_eq!(
            first,
            format!(
                "Current working directory: {}.",
                quote(&scratch.path("launch"))
            )
        );
        wd.set("s1", &scratch.path("target")).expect("set");
        let second = wd.context_text(Some("s1")).expect("context");
        assert_eq!(
            second,
            format!(
                "Current working directory: {}.",
                quote(&scratch.path("target"))
            )
        );
    }

    /// The set leg never moves the process directory or the session
    /// origin (the dsh shell/process isolation and origin-permanence
    /// clauses): the launch world and the header stay untouched.
    #[test]
    fn set_leaves_the_process_and_origin_untouched() {
        let before_process = std::env::current_dir().expect("process cwd");
        let (wd, log, _inject, scratch) = domain(
            FakeLog::with_header("/origin-repo"),
            FakeInject::default(),
            &["launch", "target"],
        );
        wd.set("s1", &scratch.path("target")).expect("set");
        assert_eq!(
            std::env::current_dir().expect("process cwd"),
            before_process
        );
        // The origin metadata rides the immutable header: no header
        // write face exists, and the log holds only the one change
        // record.
        assert_eq!(log.change_count(), 1);
    }
}
