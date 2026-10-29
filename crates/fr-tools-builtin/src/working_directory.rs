// Rust guideline compliant 2026-10-29
//! The session working directories (dsh `working-directory`, minimal
//! arm per Q16a-5A): the per-session serialized `ensure` and the
//! committed-state read.
//!
//! The filesystem inspection rides a swappable seam (the 16b migration
//! point); the durable changes ride the `'session-persistence'` faces.
//!
//! The projection registration and relay fold land here as the dsh
//! `workingDirectory` projection (stateVersion 1): the fold input is
//! the cold log the relay hands over, and the self-read path pulls the
//! log tail through the same fold kernel. Set/notice/context legs are
//! the 16f batch's scope and stay out (Q16a-5A).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;

/// The projection key (dsh `workingDirectory`).
pub const PROJECTION_KEY: &str = "workingDirectory";
/// The projection format version (dsh `stateVersion: 1`).
pub const STATE_VERSION: u64 = 1;
/// The durable change event type (dsh `working-directory/change`).
pub const CHANGE_EVENT: &str = "working-directory/change";

/// The swappable filesystem-inspection seam (the dsh `ctx.fs`
/// resolve+stat pair reduced to the directory-existence face the
/// minimal arm needs).
pub trait FsStat: Send + Sync {
    /// Whether `path` resolves to an existing directory.
    ///
    /// # Errors
    ///
    /// A failing inspection (the dsh `fs.stat` rejection family).
    fn is_directory(&self, path: &str) -> Result<bool, String>;
}

/// The std filesystem inspection (the dsh `ctx.fs` local arm).
#[derive(Debug, Default)]
pub struct StdFsStat;

impl FsStat for StdFsStat {
    fn is_directory(&self, path: &str) -> Result<bool, String> {
        Ok(std::path::Path::new(path).is_dir())
    }
}

/// The session-log seam: cold-reads one session's header plus the event
/// window the self-read folds, and appends the durable change record.
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

/// The working-directory domain (dsh working-directory minimal arm):
/// per-session serialized `ensure`, the committed read, and the relay
/// fold.
pub struct WorkingDirectories {
    /// One serialization gate per session (dsh `serialize`).
    gates: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    log: Arc<dyn SessionLog>,
    stat: Arc<dyn FsStat>,
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
    /// Assembles the domain over the log seam, the filesystem seam, and
    /// the launch directory (dsh `defaultDirectory`).
    #[must_use]
    pub fn new(log: Arc<dyn SessionLog>, stat: Arc<dyn FsStat>, default_dir: String) -> Self {
        Self {
            gates: Mutex::new(HashMap::new()),
            log,
            stat,
            default_dir,
        }
    }

    /// Reads the committed directory without filesystem I/O (dsh
    /// `get`: the projection state, else the default).
    ///
    /// # Errors
    ///
    /// The log read failure.
    pub fn get(&self, agent: &str) -> Result<String, String> {
        Ok(match self.fold_self(agent)? {
            ProjectionState::At(cwd) => cwd,
            ProjectionState::Null => self.default_dir.clone(),
        })
    }

    /// Validates the current directory and restores the original
    /// project when it disappeared (dsh `ensure` minimal arm):
    /// serialized per session, the existing directory returns as-is
    /// (committing a change record when the projection was null), a
    /// vanished directory restores `header.cwd ?? default`, and an
    /// also-vanished original refuses.
    ///
    /// # Errors
    ///
    /// The log read or append failure, a failing inspection, or the
    /// dsh recovery refusal when the original is unavailable too.
    ///
    /// # Panics
    ///
    /// Panics when the gate-table or session-gate lock is poisoned by a
    /// concurrent panic.
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
        if self.stat.is_directory(&current)? {
            // The dsh commit arm: a null projection is backfilled with a
            // change record so third-party relays converge.
            if state == ProjectionState::Null {
                self.log.append(agent, &current)?;
            }
            return Ok(current);
        }
        if !self.stat.is_directory(&original)? {
            return Err(format!(
                "working-directory: directory does not exist: {}",
                quote(&original)
            ));
        }
        // The dsh commit (recovered=true) without the notice leg (16f).
        if ProjectionState::At(original.clone()) != state {
            self.log.append(agent, &original)?;
        }
        Ok(original)
    }

    /// Pulls the log tail and folds it locally (the Q110A-prime
    /// self-read shape).
    fn fold_self(&self, agent: &str) -> Result<ProjectionState, String> {
        Ok(fold(&self.log.read(agent)?))
    }
}

/// The dsh `JSON.stringify` quoting for refusal texts.
fn quote(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One fake log: a fixed header and an appendable change list.
    #[derive(Debug, Default)]
    struct FakeLog {
        header: Mutex<Option<Value>>,
        seed: Mutex<Vec<String>>,
        changes: Mutex<Vec<String>>,
    }

    impl FakeLog {
        fn with_header(cwd: &str) -> Self {
            Self {
                header: Mutex::new(Some(serde_json::json!({ "cwd": cwd }))),
                ..Self::default()
            }
        }

        fn with_changes(paths: &[&str]) -> Self {
            Self {
                seed: Mutex::new(paths.iter().map(|path| (*path).to_owned()).collect()),
                ..Self::default()
            }
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

    /// One scripted filesystem: a fixed set of existing directories.
    #[derive(Debug, Default)]
    struct FakeStat {
        existing: Vec<String>,
    }

    impl FsStat for FakeStat {
        fn is_directory(&self, path: &str) -> Result<bool, String> {
            Ok(self.existing.iter().any(|dir| dir == path))
        }
    }

    fn domain(log: FakeLog, existing: Vec<&str>) -> (WorkingDirectories, Arc<FakeLog>) {
        let log: Arc<FakeLog> = Arc::new(log);
        let wd = WorkingDirectories::new(
            Arc::clone(&log) as Arc<dyn SessionLog>,
            Arc::new(FakeStat {
                existing: existing.into_iter().map(str::to_owned).collect(),
            }),
            "/launch".to_owned(),
        );
        (wd, log)
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
        let (wd, log) = domain(FakeLog::default(), vec!["/launch"]);
        let got = wd.ensure("s1").expect("ensure");
        assert_eq!(got, "/launch");
        // The null projection was backfilled exactly once.
        assert_eq!(log.changes.lock().expect("changes").len(), 1);
        let second = wd.ensure("s1").expect("second ensure");
        assert_eq!(second, "/launch");
        assert_eq!(log.changes.lock().expect("changes").len(), 1);
    }

    /// A vanished committed directory restores the original; an
    /// also-vanished original refuses with the dsh text.
    #[test]
    fn ensure_restores_the_original_and_refuses_when_it_is_gone_too() {
        // A committed /gone (the change log), no header cwd, default
        // /launch exists: restore.
        let (wd, log) = domain(FakeLog::with_changes(&["/gone"]), vec!["/launch"]);
        assert_eq!(wd.ensure("s1").expect("ensure"), "/launch");
        assert_eq!(log.changes.lock().expect("changes").len(), 1);
        // A vanished header cwd is also-unavailable: the dsh recovery
        // refusal (the header's own project is the restore target).
        let (wd, _log) = domain(FakeLog::with_header("/repo"), vec!["/launch"]);
        assert_eq!(
            wd.ensure("s1").unwrap_err(),
            "working-directory: directory does not exist: \"/repo\""
        );
        // Neither exists: the dsh recovery refusal.
        let (wd, _log) = domain(FakeLog::with_header("/repo"), vec![]);
        assert_eq!(
            wd.ensure("s1").unwrap_err(),
            "working-directory: directory does not exist: \"/repo\""
        );
    }
}
