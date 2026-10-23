//! The JSONL backend service (dsh index.ts `JsonlSessionPersistence`):
//! `create`/`open` returning per-session handles, the service-wide flush
//! barrier, and the `stat`/`list` discovery face.
//!
//! Sessions materialize lazily: a created session is visible to this
//! process immediately, reaches disk on its first append or flush, and
//! never existed if the process crashes before that.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use fr_wire::event::{SessionEvent, SessionHeader};
use fr_wire::ids::SessionId;

use super::filename::{self};
use super::handle::{HandleInner, JsonlSessionHandle, SessionAccess, StorageHandleState};
use super::io::FileIdentity;
use super::lease::SessionWriteLease;
use super::read::{ResolvedGeneration, resolve_generation_in_directory};
use super::scheduler::{BatchScheduler, LIVE_WRITE_BATCH_MAX_DELAY_MS};
use super::{
    JsonlCompression, PersistError, SESSION_FORMAT_VERSION, encoding_mismatch,
    session_format_version_refusal,
};
use crate::vocabulary::EventVocabulary;

/// Default physical encoding (dsh `DEFAULT_COMPRESSION`).
const DEFAULT_COMPRESSION: JsonlCompression = JsonlCompression::Zstd;

/// Maximum listing operations in flight per backend (dsh
/// `DEFAULT_LIST_CONCURRENCY`).
pub const DEFAULT_LIST_CONCURRENCY: usize = 3;

/// One stored-session observation (dsh `SessionPersistenceSnapshot`; the
/// dsh `formatStatus` field is constant for the v4-only backend and the
/// migration corpus revision has no v4-store case).
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// The stored logical header.
    pub header: SessionHeader,
    /// The best-effort change token (stat identity, or the in-memory
    /// pending token).
    pub revision: String,
    /// The physical artifact size, when the session is materialized.
    pub size_bytes: Option<u64>,
}

/// One listed artifact with its decoded header.
struct ListedArtifact {
    header: SessionHeader,
    path: PathBuf,
}

/// The JSONL backend. Shared as `Arc<JsonlBackend>`: handles and the
/// tracker hold the same instance.
pub struct JsonlBackend {
    root: PathBuf,
    compression: JsonlCompression,
    list_permits: usize,
    scheduler: BatchScheduler,
    tracker: Arc<super::tracker::BackendTracker>,
    known: EventVocabulary,
    root_encoding: Mutex<Option<Result<(), String>>>,
    warn_hook: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl fmt::Debug for JsonlBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonlBackend")
            .field("root", &self.root)
            .field("compression", &self.compression)
            .field("list_permits", &self.list_permits)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for JsonlBackendBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonlBackendBuilder")
            .field("root", &self.root)
            .field("compression", &self.compression)
            .field("list_concurrency", &self.list_concurrency)
            .field("batch_max_delay", &self.batch_max_delay)
            .finish_non_exhaustive()
    }
}

/// Builder for the backend's storage knobs (dsh `Config`: root required,
/// compression, listConcurrency).
pub struct JsonlBackendBuilder {
    root: PathBuf,
    compression: JsonlCompression,
    list_concurrency: usize,
    batch_max_delay: Duration,
    known: EventVocabulary,
    warn_hook: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

impl JsonlBackend {
    /// Starts a builder for the backend rooted at `root` with the event
    /// vocabulary that decode admits (dsh `Config.root` + catalog).
    #[must_use]
    pub fn builder(root: &Path, known: EventVocabulary) -> JsonlBackendBuilder {
        JsonlBackendBuilder {
            root: root.to_path_buf(),
            compression: DEFAULT_COMPRESSION,
            list_concurrency: DEFAULT_LIST_CONCURRENCY,
            batch_max_delay: Duration::from_millis(LIVE_WRITE_BATCH_MAX_DELAY_MS),
            known,
            warn_hook: None,
        }
    }

    /// The configured physical encoding (test access).
    #[cfg(test)]
    pub(crate) const fn compression(&self) -> JsonlCompression {
        self.compression
    }

    /// The service-wide durability barrier (dsh `flush`).
    ///
    /// # Errors
    ///
    /// Returns one aggregate naming each session whose flush failed; the
    /// remaining handles still flush.
    pub fn flush(&self) -> Result<(), PersistError> {
        self.tracker.flush_all()
    }

    /// The backend's live-routing teardown sweep (the listener wiring lands
    /// with the dougong ctx step); closes every open handle and aggregates
    /// failures (dsh `install` teardown arm).
    ///
    /// # Errors
    ///
    /// Returns the `<name> dispose failed` aggregate.
    pub fn close_all(&self) -> Result<(), PersistError> {
        self.tracker.close_all_for_dispose()
    }
}

impl Drop for JsonlBackend {
    fn drop(&mut self) {
        // Shut the batch scheduler down with the backend; later armings
        // report `false` and live routing degrades to explicit drains.
        self.scheduler.close();
    }
}

impl JsonlBackendBuilder {
    /// Selects the physical encoding (dsh `Config.compression`).
    #[must_use]
    pub const fn compression(mut self, compression: JsonlCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Bounds directory, header, and stat reads in flight across listings
    /// (dsh `Config.listConcurrency`).
    #[must_use]
    pub const fn list_concurrency(mut self, list_concurrency: usize) -> Self {
        self.list_concurrency = list_concurrency;
        self
    }

    /// Overrides the live batch window (production keeps the dsh 200 ms;
    /// tests shorten it).
    #[must_use]
    pub const fn batch_max_delay(mut self, batch_max_delay: Duration) -> Self {
        self.batch_max_delay = batch_max_delay;
        self
    }

    /// Installs the warn hook for background drain failures (dsh
    /// `ctx.logger.warn`; the dougong ctx step supplies the real one).
    #[must_use]
    pub fn warn_hook(mut self, hook: Arc<dyn Fn(String) + Send + Sync>) -> Self {
        self.warn_hook = Some(hook);
        self
    }

    /// Builds the shared backend (dsh constructor: root resolution,
    /// defaults, `assertUsableRoot`).
    ///
    /// # Errors
    ///
    /// Returns `Io` for an existing-but-unreadable root or a failed
    /// scheduler spawn.
    pub fn build(self) -> Result<Arc<JsonlBackend>, PersistError> {
        match std::fs::read_dir(&self.root) {
            // Only an absent root is fine: it is created on first
            // materialization; an existing root must be readable.
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.into());
            }
            _ => {}
        }
        let scheduler = BatchScheduler::spawn(self.batch_max_delay)?;
        // Resolve once so later cwd changes cannot split one backend
        // across roots (dsh `resolve(config.root)`).
        let root = if self.root.is_absolute() {
            self.root
        } else {
            std::env::current_dir()
                .map_err(PersistError::Io)?
                .join(&self.root)
        };
        Ok(Arc::new(JsonlBackend {
            root,
            compression: self.compression,
            list_permits: self.list_concurrency.max(1),
            scheduler,
            tracker: Arc::new(super::tracker::BackendTracker::new(
                "session-persistence-jsonl",
            )),
            known: self.known,
            root_encoding: Mutex::new(None),
            warn_hook: self.warn_hook,
        }))
    }
}

impl JsonlBackend {
    /// Creates a new stored session and takes its write ownership (dsh
    /// `create`): visible to this process immediately, the physical
    /// artifact appears on the first append or flush.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a `createdAt` outside the safe-integer
    /// domain or a seeded/cut mismatch, `AlreadyExists` for an occupied
    /// id, and the root-encoding refusals.
    pub fn create(
        self: &Arc<Self>,
        header: &SessionHeader,
        inherited_event_count: u64,
    ) -> Result<JsonlSessionHandle, PersistError> {
        let snapshot = materialize_create_header(header)?;
        // Fail fast on a seeded/cut mismatch with the exact refusal the
        // header line encoder enforces at materialization.
        super::header_line::to_header_line(
            &snapshot,
            snapshot.is_seeded.then_some(inherited_event_count),
        )?;
        self.ensure_root_encoding()?;
        if self.tracker.has_pending(&snapshot.id) || self.find_log(&snapshot.id)?.is_some() {
            return Err(PersistError::AlreadyExists(format!(
                "session \"{}\" already exists",
                snapshot.id.as_str()
            )));
        }
        // No lock yet: before materialization there is no durable artifact
        // for another process to contend over, so the handle acquires the
        // lock right before its first log bytes publish; an unmaterialized
        // session leaves no filesystem footprint at all.
        self.tracker
            .register_created(&snapshot, inherited_event_count)?;
        let handle = JsonlSessionHandle::new(
            Arc::clone(self),
            snapshot,
            SessionAccess::Write,
            StorageHandleState {
                cursor: 0,
                materialized: false,
                torn_truncate_to: None,
                recovered_tail: None,
                inherited_event_count,
                primed: None,
                lease: None,
            },
            self.scheduler.clone(),
            self.warn_hook.clone(),
        )?;
        self.tracker.adopt(&handle);
        Ok(handle)
    }

    /// Opens an existing stored session for `read` or single-writer
    /// `write` (dsh `open`).
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for an absent id, `AlreadyOwned` for a taken
    /// write claim, `Unsupported` for a foreign generation, and the
    /// storage/encoding refusals.
    pub fn open(
        self: &Arc<Self>,
        id: &SessionId,
        access: SessionAccess,
    ) -> Result<JsonlSessionHandle, PersistError> {
        self.ensure_root_encoding()?;
        match access {
            SessionAccess::Read => self.open_read(id, access),
            SessionAccess::Write => self.open_write(id),
        }
    }

    fn open_read(
        self: &Arc<Self>,
        id: &SessionId,
        access: SessionAccess,
    ) -> Result<JsonlSessionHandle, PersistError> {
        if let Some(pending) = self.tracker.pending_of(id) {
            let state = StorageHandleState {
                cursor: 0,
                materialized: false,
                torn_truncate_to: None,
                recovered_tail: None,
                inherited_event_count: pending.inherited_event_count,
                primed: None,
                lease: None,
            };
            let handle = JsonlSessionHandle::new(
                Arc::clone(self),
                pending.header,
                access,
                state,
                self.scheduler.clone(),
                self.warn_hook.clone(),
            )?;
            self.tracker.adopt(&handle);
            return Ok(handle);
        }
        let selected = self.find_log(id)?.ok_or_else(|| not_found(id))?;
        Self::generation_version_gate(id, &selected)?;
        let stored = self.require_stored_log(id, &selected)?;
        let state = StorageHandleState {
            cursor: 0,
            materialized: true,
            torn_truncate_to: None,
            recovered_tail: None,
            inherited_event_count: stored.inherited_event_count,
            primed: Some(stored.events),
            lease: None,
        };
        let handle = JsonlSessionHandle::new(
            Arc::clone(self),
            stored.meta,
            access,
            state,
            self.scheduler.clone(),
            self.warn_hook.clone(),
        )?;
        self.tracker.adopt(&handle);
        Ok(handle)
    }

    fn open_write(self: &Arc<Self>, id: &SessionId) -> Result<JsonlSessionHandle, PersistError> {
        // A pending entry always belongs to an ACTIVE creator handle (its
        // close erases it), so this claim rejects that case as already
        // owned (dsh claimWrite before the I/O).
        self.tracker.claim_write(id)?;
        let mut lease: Option<SessionWriteLease> = None;
        let opened = self.finish_write_open(id, &mut lease);
        match opened {
            Ok(handle) => {
                self.tracker.adopt(&handle);
                Ok(handle)
            }
            Err(failure) => {
                // Free the in-process claim no matter how the kernel-lock
                // release fares, and keep the original diagnostic: a
                // release failure joins it instead of replacing it.
                let release_failure = lease.take().and_then(|mut held| held.release().err());
                self.tracker.release_claim(id);
                if let Some(release_error) = release_failure {
                    return Err(PersistError::Aggregate {
                        message: format!(
                            "session \"{}\": write open failed and its lock release failed",
                            id.as_str()
                        ),
                        errors: vec![failure, release_error],
                    });
                }
                Err(failure)
            }
        }
    }

    fn finish_write_open(
        self: &Arc<Self>,
        id: &SessionId,
        lease: &mut Option<SessionWriteLease>,
    ) -> Result<JsonlSessionHandle, PersistError> {
        let selected = self.find_log(id)?.ok_or_else(|| not_found(id))?;
        Self::generation_version_gate(id, &selected)?;
        *lease = Some(SessionWriteLease::acquire(
            selected
                .source_path
                .parent()
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
                .as_path(),
            id,
        )?);
        // Re-read under the lock: a writer that finished between find_log
        // and the lock left a newer tail this open must see.
        let stored = self.require_stored_log(id, &selected)?;
        let state = StorageHandleState {
            cursor: stored.events.len() as u64,
            materialized: true,
            torn_truncate_to: stored.torn_truncate_to,
            recovered_tail: Some(stored.recovered_tail),
            inherited_event_count: stored.inherited_event_count,
            primed: Some(stored.events),
            lease: lease.take(),
        };
        JsonlSessionHandle::new(
            Arc::clone(self),
            stored.meta,
            SessionAccess::Write,
            state,
            self.scheduler.clone(),
            self.warn_hook.clone(),
        )
    }

    /// Observes one stored session without reading its event log (dsh
    /// `stat`): the snapshot, or `None` when the session does not exist.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for a foreign generation and the storage
    /// refusals.
    pub fn stat(&self, id: &SessionId) -> Result<Option<SessionSnapshot>, PersistError> {
        self.ensure_root_encoding()?;
        if let Some(pending) = self.tracker.pending_of(id) {
            return Ok(Some(SessionSnapshot {
                header: pending.header,
                revision: pending.revision,
                size_bytes: None,
            }));
        }
        let Some(selected) = self.find_log(id)? else {
            return Ok(None);
        };
        Self::generation_version_gate(id, &selected)?;
        let Some(header) = self.read_generation_header(&selected, Some(id))? else {
            return Ok(None);
        };
        let identity = match super::io::file_identity(&selected.source_path) {
            Ok(identity) => identity,
            Err(error) if error.is_enoent() => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(Some(SessionSnapshot {
            header,
            revision: identity.revision(),
            size_bytes: Some(identity.size),
        }))
    }

    /// Lists every stored session visible to this process (dsh `list`):
    /// materialized artifacts plus this process's created-but-unmaterialized
    /// sessions, in no promised order.
    ///
    /// # Errors
    ///
    /// Returns the discovery refusals (legacy layout, duplicate ids,
    /// encoding mismatch) and the storage failures.
    pub fn list(&self) -> Result<Vec<SessionSnapshot>, PersistError> {
        // Snapshot pending entries BEFORE scanning storage: a session whose
        // first append lands mid-scan is still in this snapshot, so
        // create-to-list visibility never has a hole.
        let pending = self.tracker.pending_entries();
        let artifacts = self.list_artifacts()?;
        let mut snapshots = Vec::new();
        let mut listed = HashSet::new();
        for artifact in artifacts {
            let identity = match super::io::file_identity(&artifact.path) {
                Ok(identity) => identity,
                Err(error) if error.is_enoent() => continue,
                Err(error) => return Err(error),
            };
            listed.insert(artifact.header.id.clone());
            snapshots.push(SessionSnapshot {
                header: artifact.header,
                revision: identity.revision(),
                size_bytes: Some(identity.size),
            });
        }
        for (id, entry) in pending {
            if !listed.contains(&id) {
                snapshots.push(SessionSnapshot {
                    header: entry.header,
                    revision: entry.revision,
                    size_bytes: None,
                });
            }
        }
        Ok(snapshots)
    }
}

impl JsonlBackend {
    /// Enumerates stored artifacts with decoded headers (dsh
    /// `listArtifacts`): unreadable headers skip their entry; a duplicate
    /// id across project directories is a hard error.
    fn list_artifacts(&self) -> Result<Vec<ListedArtifact>, PersistError> {
        self.ensure_root_encoding()?;
        let mut artifacts = Vec::new();
        let mut ids = HashSet::new();
        for selected in self.list_generations()? {
            match self.read_generation_header(&selected, None) {
                Ok(Some(header)) => {
                    if !ids.insert(header.id.clone()) {
                        return Err(PersistError::Corruption(format!(
                            "duplicate JSONL session id \"{}\" appears in multiple \
project directories",
                            header.id.as_str()
                        )));
                    }
                    artifacts.push(ListedArtifact {
                        header,
                        path: selected.source_path,
                    });
                }
                // Unsupported or corrupt headers skip their entry; other
                // failures surface.
                Ok(None) => {}
                Err(error @ (PersistError::Unsupported(_) | PersistError::Corruption(_))) => {
                    drop(error);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(artifacts)
    }

    /// Enumerates selected physical generations without interpreting their
    /// headers (dsh `listGenerations`).
    fn list_generations(&self) -> Result<Vec<ResolvedGeneration>, PersistError> {
        let selections = self.map_session_directories(|dir| {
            resolve_generation_in_directory(dir, self.compression)
        })?;
        Ok(selections.into_iter().flatten().collect())
    }

    /// Applies `task` to every session directory through the shared listing
    /// bound (dsh `mapSessionDirectories`): results and the first failure
    /// follow the sequential project-then-directory order; a project whose
    /// listing fails ends the traversal after the directories of the
    /// projects before it.
    fn map_session_directories<T, F>(&self, task: F) -> Result<Vec<T>, PersistError>
    where
        F: Fn(&Path) -> Result<T, PersistError> + Sync,
        T: Send,
    {
        // The root read is one operation and cannot contend with itself,
        // so it needs no permit; the bound bites in the phases below.
        let project_dirs = self.list_project_dirs()?;
        let project_outcomes = run_bounded(self.list_permits, &project_dirs, |project| {
            Self::list_session_dirs(project.as_path())
        });
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut project_failure = None;
        for outcome in project_outcomes {
            match outcome {
                Ok(entries) => dirs.extend(entries),
                Err(error) => {
                    project_failure = Some(error);
                    break;
                }
            }
        }
        let results = run_bounded(self.list_permits, &dirs, |dir: &PathBuf| {
            task(dir.as_path())
        });
        // First failure in input order, then the deferred project failure
        // (dsh settleLimited + settledValues ordering).
        let results: Vec<T> = results.into_iter().collect::<Result<Vec<_>, _>>()?;
        if let Some(error) = project_failure {
            return Err(error);
        }
        Ok(results)
    }
}

/// Runs `task` over `items` with at most `permits` executions in flight,
/// returning one outcome per item in input order (dsh
/// `settleLimited(pLimit(n), ...)`).
fn run_bounded<I, T, F>(permits: usize, items: &[I], task: F) -> Vec<Result<T, PersistError>>
where
    I: Sync,
    T: Send,
    F: Fn(&I) -> Result<T, PersistError> + Sync,
{
    let available = AtomicUsize::new(permits.max(1));
    let gate = Mutex::new(());
    let signal = Condvar::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for item in items {
            let mut gate_guard = super::handle::lock(&gate);
            while available.load(Ordering::Acquire) == 0 {
                gate_guard = signal
                    .wait(gate_guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            available.fetch_sub(1, Ordering::AcqRel);
            drop(gate_guard);
            handles.push(scope.spawn({
                let task = &task;
                let available = &available;
                let signal = &signal;
                move || {
                    let outcome = task(item);
                    available.fetch_add(1, Ordering::AcqRel);
                    signal.notify_one();
                    outcome
                }
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|panic| {
                    Err(PersistError::Corruption(format!(
                        "session listing worker panicked: {panic:?}"
                    )))
                })
            })
            .collect()
    })
}

impl JsonlBackend {
    /// The human-readable project directories under the configured root
    /// (dsh `listProjectDirs`): only an absent root means no sessions.
    fn list_project_dirs(&self) -> Result<Vec<PathBuf>, PersistError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        };
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(PersistError::Io)?;
            if entry.file_type().map_err(PersistError::Io)?.is_dir() {
                dirs.push(entry.path());
            }
        }
        Ok(dirs)
    }

    /// Lists session-owned directories and rejects the obsolete flat-file
    /// layout (dsh `listSessionDirs`).
    fn list_session_dirs(project: &Path) -> Result<Vec<PathBuf>, PersistError> {
        let entries = std::fs::read_dir(project).map_err(PersistError::Io)?;
        let mut dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(PersistError::Io)?;
            let file_type = entry.file_type().map_err(PersistError::Io)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_file() && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zstd")) {
                return Err(Self::legacy_layout(&entry.path()));
            }
            if file_type.is_dir() {
                dirs.push(entry.path());
            }
        }
        Ok(dirs)
    }

    /// The dsh `legacyLayout` refusal text.
    fn legacy_layout(path: &Path) -> PersistError {
        PersistError::Corruption(format!(
            "session artifact {} uses the unsupported flat-file layout; use a separate \
root or move it into a project/session directory before loading",
            serde_json::to_string(&path.display().to_string()).unwrap_or_default(),
        ))
    }
}

impl JsonlBackend {
    /// Finds the unique authoritative generation for an id across project
    /// directories (dsh `findLog`).
    fn find_log(&self, id: &SessionId) -> Result<Option<ResolvedGeneration>, PersistError> {
        let mut matches = Vec::new();
        for project in self.list_project_dirs()? {
            Self::reject_legacy_flat(project.as_path(), id)?;
            let encoded = super::paths::encode_segment(id.as_str())?;
            let dir = project.join(encoded);
            if let Some(selected) = resolve_generation_in_directory(&dir, self.compression)? {
                matches.push(selected);
            }
        }
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches.remove(0))),
            _ => Err(PersistError::Corruption(format!(
                "duplicate JSONL session id \"{}\" appears in multiple project directories",
                id.as_str()
            ))),
        }
    }

    /// Rejects the obsolete flat-file layout for one id (dsh
    /// `rejectLegacyFlatArtifact`).
    fn reject_legacy_flat(project: &Path, id: &SessionId) -> Result<(), PersistError> {
        let encoded = super::paths::encode_segment(id.as_str())?;
        for compression in [JsonlCompression::Zstd, JsonlCompression::None] {
            let path = project.join(format!("{encoded}{}", filename::log_suffix(compression)));
            if probe_exists(&path)? {
                return Err(Self::legacy_layout(&path));
            }
        }
        Ok(())
    }

    /// Rejects a root that already holds the other physical encoding's
    /// artifacts (dsh `ensureRootEncoding`), memoizing the deterministic
    /// refusal.
    fn ensure_root_encoding(&self) -> Result<(), PersistError> {
        {
            let memo = super::handle::lock(&self.root_encoding);
            match memo.as_ref() {
                Some(Ok(())) => return Ok(()),
                Some(Err(message)) => {
                    return Err(PersistError::Corruption(message.clone()));
                }
                None => {}
            }
        }
        let outcome = self.check_root_encoding();
        let mut memo = super::handle::lock(&self.root_encoding);
        match &outcome {
            Ok(()) => *memo = Some(Ok(())),
            // Only the deterministic encoding refusal memoizes; a transient
            // I/O failure repeating on the next call is strictly more
            // useful than caching it.
            Err(error) if matches!(error, PersistError::Corruption(_)) => {
                *memo = Some(Err(error.message().to_owned()));
            }
            Err(_) => {}
        }
        outcome
    }

    fn check_root_encoding(&self) -> Result<(), PersistError> {
        self.map_session_directories(|dir| {
            if let Some(path) = find_opposite_generation_in_directory(dir, self.compression)? {
                return Err(encoding_mismatch(&path, self.compression));
            }
            Ok(())
        })
        .map(|_per_dir| ())
    }
}

/// Whether one path exists; every non-`ENOENT` failure surfaces (dsh
/// `exists`).
fn probe_exists(path: &Path) -> Result<bool, PersistError> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The highest canonical generation encoded with the opposite compression
/// in one session directory (dsh `findOppositeGenerationInDirectory`).
fn find_opposite_generation_in_directory(
    dir: &Path,
    compression: JsonlCompression,
) -> Result<Option<PathBuf>, PersistError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let opposite = match compression {
        JsonlCompression::Zstd => JsonlCompression::None,
        JsonlCompression::None => JsonlCompression::Zstd,
    };
    let mut latest: Option<(u64, PathBuf)> = None;
    for entry in entries {
        let entry = entry.map_err(PersistError::Io)?;
        let name = entry.file_name();
        let Some(version) =
            filename::parse_generation_log_filename(&name.to_string_lossy(), opposite)
        else {
            continue;
        };
        if latest.as_ref().is_none_or(|(known, _)| version > *known) {
            latest = Some((version, entry.path()));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

impl JsonlBackend {
    /// Resolves and reads one stored log, refusing loudly when the artifact
    /// is absent (dsh `requireStoredLog`).
    fn require_stored_log(
        &self,
        id: &SessionId,
        selected: &ResolvedGeneration,
    ) -> Result<super::read::ReadOutcome, PersistError> {
        Self::generation_version_gate(id, selected)?;
        let (raw, _identity) = read_stable_bytes(&selected.source_path)?;
        super::read::decode_stored_log(
            &self.root,
            &selected.source_path,
            &raw,
            id,
            self.compression,
            self.known.clone(),
        )
    }

    /// The migration-excluded version gate: every non-current generation
    /// refuses with the direction-aware dsh text plus its raw-log path
    /// (PRD 19.1 excludes the v0-v3 migration chain).
    fn generation_version_gate(
        id: &SessionId,
        selected: &ResolvedGeneration,
    ) -> Result<(), PersistError> {
        if selected.source_version == SESSION_FORMAT_VERSION {
            return Ok(());
        }
        Err(PersistError::Unsupported(format!(
            "{} (raw log: {})",
            session_format_version_refusal(id.as_str(), selected.source_version),
            selected.source_path.display()
        )))
    }

    /// Reads and translates one selected generation header without
    /// inspecting its body (dsh `readGenerationHeader`): `Ok(None)` marks a
    /// malformed or absent header.
    fn read_generation_header(
        &self,
        selected: &ResolvedGeneration,
        expected_id: Option<&SessionId>,
    ) -> Result<Option<SessionHeader>, PersistError> {
        let first = match self.compression {
            JsonlCompression::Zstd => Self::read_first_zstd_line(&selected.source_path)?,
            JsonlCompression::None => read_first_line(&selected.source_path)?,
        };
        let Some(first) = first else {
            return Ok(None);
        };
        let value: serde_json::Value = match serde_json::from_str(&first) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        super::header_line::assert_no_retired_header_fields(&value)?;
        let mut record = first.into_bytes();
        record.push(0x0A);
        let header = match super::header_line::parse_header_record(&record) {
            Ok(header) => header,
            Err(PersistError::Unsupported(reason)) => {
                return Err(PersistError::Unsupported(format!(
                    "{reason} (raw log: {})",
                    selected.source_path.display()
                )));
            }
            Err(_) => return Ok(None),
        };
        if header.version != selected.source_version {
            return Err(PersistError::Corruption(format!(
                "session generation filename identifies v{}, but its header identifies v{}",
                selected.source_version, header.version
            )));
        }
        super::read::assert_stored_identity(
            &self.root,
            &selected.source_path,
            selected.source_version,
            &header,
            expected_id,
            self.compression,
        )?;
        Ok(Some(header))
    }

    /// Reads only the first header frame; compression failures reject as
    /// corruption (dsh `readFirstZstdLine`).
    fn read_first_zstd_line(path: &Path) -> Result<Option<String>, PersistError> {
        let mut content = Vec::new();
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        std::io::Read::read_to_end(&mut file, &mut content).map_err(PersistError::Io)?;
        let frames = super::zstd::scan_zstd_frames(&content, Some(1))?;
        let Some(first) = frames.frames.first() else {
            return Ok(None);
        };
        let plaintext = super::zstd::decompress_zstd_frame(&content[first.start..first.end])
            .map_err(|error| {
                PersistError::Corruption(format!(
                    "corrupt Zstandard session log: header frame failed validation: \
{error} (raw log: {})",
                    path.display()
                ))
            })?;
        // The header frame is exactly one newline-terminated record (dsh
        // `assertZstdHeaderFrame`).
        let Some(newline) = plaintext.iter().position(|byte| *byte == 0x0A_u8) else {
            return Err(PersistError::Corruption(format!(
                "corrupt Zstandard session log: header frame failed validation: \
the first frame holds no complete header record (raw log: {})",
                path.display()
            )));
        };
        let (line, tail) = plaintext.split_at(newline);
        let tail = &tail[1..];
        if tail.contains(&0x0A_u8) || !tail.is_empty() {
            return Err(PersistError::Corruption(format!(
                "corrupt Zstandard session log: header frame failed validation: \
the first frame holds more than one record (raw log: {})",
                path.display()
            )));
        }
        Ok(Some(String::from_utf8_lossy(line).into_owned()))
    }

    /// Handle-facing storage: appends encoded lines or materializes the
    /// first publication (dsh `JsonlHandleStorage.persistBatch`).
    pub(crate) fn persist_batch(
        &self,
        header: &SessionHeader,
        events: &[SessionEvent],
        is_materialized: bool,
        inherited_event_count: u64,
    ) -> Result<(), PersistError> {
        if is_materialized {
            let path = filename::log_path(
                &self.root,
                header.cwd.as_deref(),
                &header.id,
                self.compression,
            )?;
            super::io::append_lines(&path, events, self.compression)
        } else {
            super::io::materialize(
                &self.root,
                header,
                inherited_event_count,
                events,
                self.compression,
            )?;
            self.tracker.materialized(&header.id);
            Ok(())
        }
    }

    /// Handle-facing storage: materializes the header-only artifact for an
    /// explicitly flushed empty session (dsh `persistHeader`).
    pub(crate) fn persist_header(
        &self,
        header: &SessionHeader,
        inherited_event_count: u64,
    ) -> Result<(), PersistError> {
        super::io::materialize(
            &self.root,
            header,
            inherited_event_count,
            &[],
            self.compression,
        )?;
        self.tracker.materialized(&header.id);
        Ok(())
    }

    /// Handle-facing storage: truncates a torn physical tail before the
    /// first new append lands (dsh `truncateTornTail`).
    pub(crate) fn truncate_torn_tail(
        &self,
        header: &SessionHeader,
        truncate_to: u64,
    ) -> Result<(), PersistError> {
        let path = filename::log_path(
            &self.root,
            header.cwd.as_deref(),
            &header.id,
            self.compression,
        )?;
        super::io::repair(&path, truncate_to)
    }

    /// Handle-facing storage: resolves the current-generation artifact
    /// path (dsh `resolveCurrentLog`).
    pub(crate) fn resolve_current_log(
        &self,
        id: &SessionId,
    ) -> Result<Option<PathBuf>, PersistError> {
        Ok(self.find_log(id)?.map(|selected| selected.source_path))
    }

    /// Handle-facing storage: whether the id is still a
    /// created-but-unmaterialized session here (dsh `hasPendingSession`).
    pub(crate) fn has_pending_session(&self, id: &SessionId) -> bool {
        self.tracker.has_pending(id)
    }

    /// Handle-facing storage: acquires the session's cross-process write
    /// lock for a materializing created session (dsh
    /// `acquireWriteLease`), refusing an opposite-encoding artifact first.
    pub(crate) fn acquire_write_lease(
        &self,
        header: &SessionHeader,
    ) -> Result<SessionWriteLease, PersistError> {
        let dir = filename::session_dir(&self.root, header.cwd.as_deref(), &header.id)?;
        if let Some(path) = find_opposite_generation_in_directory(&dir, self.compression)? {
            return Err(encoding_mismatch(&path, self.compression));
        }
        SessionWriteLease::acquire(&dir, &header.id)
    }

    /// Handle-facing storage: reads and validates the stored log at `path`
    /// (dsh `readStoredLog`), through the stable-read discipline.
    pub(crate) fn read_stored_log_for_handle(
        &self,
        id: &SessionId,
        path: &Path,
    ) -> Result<super::read::ReadOutcome, PersistError> {
        let (raw, _identity) = read_stable_bytes(path)?;
        super::read::decode_stored_log(
            &self.root,
            path,
            &raw,
            id,
            self.compression,
            self.known.clone(),
        )
    }

    /// Handle-facing storage: drops the handle's bookkeeping on close (dsh
    /// `releaseHandle`).
    pub(crate) fn release_handle(&self, inner: &HandleInner, materialized: bool) {
        self.tracker
            .release(inner.uid, &inner.id, inner.access, materialized);
    }
}

/// Reads one stable revision of a JSONL artifact with a single retry; if
/// an append overlaps both reads, the second read's committed pre-read
/// prefix returns instead of starving behind a continuous writer (dsh
/// `readStableJsonlFile`, generation.ts:255-285).
fn read_stable_bytes(path: &Path) -> Result<(Vec<u8>, FileIdentity), PersistError> {
    let mut before = super::io::file_identity(path)?;
    let mut attempt = 0u8;
    loop {
        let mut bytes = std::fs::read(path).map_err(PersistError::Io)?;
        let after = super::io::file_identity(path)?;
        if before == after {
            return Ok((bytes, after));
        }
        if attempt >= 1 {
            let committed = usize::try_from(before.size).unwrap_or(usize::MAX);
            bytes.truncate(committed);
            return Ok((bytes, before));
        }
        attempt += 1;
        before = after;
    }
}

/// Reads the first newline-terminated line of a plaintext artifact;
/// `Ok(None)` marks absence or an incomplete final line (dsh
/// `readFirstLine`).
fn read_first_line(path: &Path) -> Result<Option<String>, PersistError> {
    use std::io::Read as _;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut chunk = [0_u8; 8192];
    let mut line = Vec::new();
    loop {
        let read = file.read(&mut chunk).map_err(PersistError::Io)?;
        if read == 0 {
            // EOF with no newline: no complete line.
            return Ok(None);
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == 0x0A) {
            line.extend_from_slice(&chunk[..newline]);
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        line.extend_from_slice(&chunk[..read]);
    }
}

/// Validates and snapshots a header passed to `create` in one pass (dsh
/// `materializeCreateHeader`): the typed header is always losslessly
/// JSON-serializable, leaving the `createdAt` domain check.
fn materialize_create_header(header: &SessionHeader) -> Result<SessionHeader, PersistError> {
    if header.created_at < 0
        || i128::from(header.created_at) > i128::from(fr_wire::ids::SAFE_INTEGER_MAX)
    {
        return Err(PersistError::InvalidInput(
            "session metadata createdAt must be a non-negative safe integer".to_owned(),
        ));
    }
    Ok(header.clone())
}

/// The absent-session refusal (dsh `SessionPersistenceNotFoundError`).
fn not_found(id: &SessionId) -> PersistError {
    PersistError::NotFound(format!("session \"{}\" not found", id.as_str()))
}

// Rust guideline compliant 2026-10-23

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::paths;
    use fr_wire::event::{
        SessionEventData, TurnEndData, TurnEndReason, TurnEndReasonKind, TurnStartData,
    };
    use fr_wire::ids::SessionSeq;
    use std::os::unix::fs::PermissionsExt as _;

    /// The dsh "rest of the log" default length (`MAX_SAFE_INTEGER`).
    const ALL: u64 = fr_wire::ids::SAFE_INTEGER_MAX;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-session-backend-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos())
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    use std::fs;

    fn backend(tag: &str) -> (TempRoot, Arc<JsonlBackend>) {
        let root = TempRoot::new(tag);
        let backend = JsonlBackend::builder(root.path(), EventVocabulary::core())
            .batch_max_delay(Duration::from_millis(30))
            .build()
            .expect("backend builds");
        (root, backend)
    }

    fn header(id: &str) -> SessionHeader {
        SessionHeader {
            version: SESSION_FORMAT_VERSION,
            id: SessionId::new(id.to_owned()),
            created_at: 1,
            cwd: None,
            parent_session: None,
            is_seeded: false,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    fn event(seq: u64, data: SessionEventData) -> SessionEvent {
        SessionEvent {
            seq: SessionSeq::new(seq).expect("seq"),
            time: 1_000 + i64::try_from(seq).expect("seq fits i64"),
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn turn(seq: u64, index: u64) -> Vec<SessionEvent> {
        vec![
            event(
                seq,
                SessionEventData::TurnStart(TurnStartData { turn: index }),
            ),
            event(
                seq + 1,
                SessionEventData::TurnEnd(TurnEndData {
                    turn: index,
                    reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
                }),
            ),
        ]
    }

    fn unique_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        format!("{nanos:x}")
    }

    #[test]
    fn create_round_trips_and_lists() {
        let (root, backend) = backend("round-trip");
        let _keep = &root;
        let handle = backend.create(&header("s-roundtrip"), 0).expect("create");
        let batch = turn(0, 1);
        handle.append(&batch).expect("append");
        handle.flush().expect("flush");
        assert_eq!(handle.read(0, ALL).expect("read").events, batch);
        let snapshot = backend
            .stat(handle.id())
            .expect("stat")
            .expect("session exists");
        assert_eq!(snapshot.header.id, *handle.id());
        assert!(snapshot.size_bytes.expect("materialized") > 0);
        let listed = backend.list().expect("list");
        assert!(listed.iter().any(|snap| snap.header.id == *handle.id()));
    }

    #[test]
    fn unmaterialized_create_is_visible_and_reads_detached() {
        let (root, backend) = backend("pending");
        let _keep = &root;
        let handle = backend.create(&header("s-pending"), 0).expect("create");
        let snapshot = backend.stat(handle.id()).expect("stat").expect("pending");
        assert!(snapshot.size_bytes.is_none());
        assert!(
            snapshot
                .revision
                .starts_with("memory:session-persistence-jsonl:")
        );
        let listed = backend.list().expect("list");
        assert!(listed.iter().any(|snap| snap.header.id == *handle.id()));
        // Reads before the first append see the empty detached log.
        let reader = handle.read(0, ALL).expect("read");
        assert_eq!(reader.events.len(), 0);
        // A read handle opened on the pending session reads the same.
        let other = backend
            .open(handle.id(), SessionAccess::Read)
            .expect("read open");
        assert_eq!(other.read(0, ALL).expect("read").events.len(), 0);
    }

    #[test]
    fn create_duplicate_refuses_verbatim() {
        let (root, backend) = backend("dup");
        let _keep = &root;
        let id = format!("s-dup-{}", unique_id());
        backend.create(&header(&id), 0).expect("create");
        let error = backend
            .create(&header(&id), 0)
            .expect_err("duplicate refuses");
        assert_eq!(error.message(), format!("session \"{id}\" already exists"));
    }

    #[test]
    fn write_open_twice_refuses_already_owned() {
        let (root, backend) = backend("owned");
        let _keep = &root;
        let id = format!("s-owned-{}", unique_id());
        {
            let handle = backend.create(&header(&id), 0).expect("create");
            handle.append(&turn(0, 1)).expect("append");
            handle.flush().expect("flush");
            // Close releases the in-process claim; the handle drops.
            handle.close().expect("close");
        }
        let first = backend
            .open(&SessionId::new(id.clone()), SessionAccess::Write)
            .expect("write open");
        let error = backend
            .open(&SessionId::new(id.clone()), SessionAccess::Write)
            .expect_err("second claim refuses");
        assert_eq!(
            error.message(),
            format!("session \"{id}\" is already owned by an active write handle")
        );
        first.close().expect("close");
        // The freed claim opens again.
        let again = backend
            .open(&SessionId::new(id), SessionAccess::Write)
            .expect("reopen");
        again.close().expect("close");
    }

    #[test]
    fn missing_session_refuses_not_found_verbatim() {
        let (root, backend) = backend("missing");
        let _keep = &root;
        let id = SessionId::new(format!("s-absent-{}", unique_id()));
        for access in [SessionAccess::Read, SessionAccess::Write] {
            let error = backend.open(&id, access).expect_err("absent id refuses");
            assert_eq!(
                error.message(),
                format!("session \"{}\" not found", id.as_str())
            );
        }
        assert!(backend.stat(&id).expect("stat").is_none());
    }

    #[test]
    fn contiguity_refusal_is_verbatim() {
        let (root, backend) = backend("contiguity");
        let _keep = &root;
        let handle = backend
            .create(&header(&format!("s-c-{}", unique_id())), 0)
            .expect("create");
        let mut batch = turn(0, 1);
        batch[1].seq = SessionSeq::new(5).expect("seq");
        let error = handle.append(&batch).expect_err("non-contiguous refuses");
        let id = handle.id().as_str().to_owned();
        assert_eq!(
            error.message(),
            format!("append seq mismatch for \"{id}\": expected 1 at index 1, got 5")
        );
    }

    #[test]
    fn read_only_handle_refuses_mutations_verbatim() {
        let (root, backend) = backend("readonly");
        let _keep = &root;
        let handle = backend
            .create(&header(&format!("s-ro-{}", unique_id())), 0)
            .expect("create");
        handle.append(&turn(0, 1)).expect("append");
        handle.flush().expect("flush");
        handle.close().expect("close");
        let reader = backend
            .open(handle.id(), SessionAccess::Read)
            .expect("read open");
        let append_error = reader.append(&turn(2, 0)).expect_err("append refuses");
        assert_eq!(
            append_error.message(),
            format!(
                "session \"{}\": append is not available on a read handle",
                reader.id().as_str()
            )
        );
        let flush_error = reader.flush().expect_err("flush refuses");
        assert_eq!(
            flush_error.message(),
            format!(
                "session \"{}\": flush is not available on a read handle",
                reader.id().as_str()
            )
        );
    }

    #[test]
    fn closed_handle_refuses_before_argument_validation() {
        let (root, backend) = backend("closed");
        let _keep = &root;
        let handle = backend
            .create(&header(&format!("s-x-{}", unique_id())), 0)
            .expect("create");
        handle.close().expect("close");
        // A bogus slice window must still refuse as closed first.
        let error = handle
            .read(u64::MAX, u64::MAX)
            .expect_err("closed read refuses");
        assert_eq!(
            error.message(),
            format!(
                "session \"{}\": read on a closed handle",
                handle.id().as_str()
            )
        );
        // Close is idempotent with one shared outcome.
        handle.close().expect("second close shares the outcome");
    }

    #[test]
    fn read_offset_domain_refusal_is_verbatim() {
        let (root, backend) = backend("offset");
        let _keep = &root;
        let handle = backend
            .create(&header(&format!("s-o-{}", unique_id())), 0)
            .expect("create");
        let overflow = fr_wire::ids::SAFE_INTEGER_MAX + 1;
        let error = handle.read(overflow, 1).expect_err("unsafe offset refuses");
        assert_eq!(
            error.message(),
            format!("read offset must be a non-negative safe integer, got {overflow}")
        );
        let error = handle.read(0, overflow).expect_err("unsafe length refuses");
        assert_eq!(
            error.message(),
            format!("read length must be a non-negative safe integer, got {overflow}")
        );
    }

    #[test]
    fn monotonic_view_refuses_shrunk_log() {
        let (root, backend) = backend("monotonic");
        let handle = backend
            .create(&header(&format!("s-m-{}", unique_id())), 0)
            .expect("create");
        handle.append(&turn(0, 1)).expect("append");
        handle.flush().expect("flush");
        assert_eq!(handle.read(0, ALL).expect("read").events.len(), 2);
        // Externally shrink the artifact to a valid header-only log; the
        // decode then succeeds with fewer events than observed.
        let path = filename::log_path(root.path(), None, handle.id(), backend.compression())
            .expect("path");
        let header_json = serde_json::to_string(
            &crate::persist::header_line::to_header_line(handle.header(), None)
                .expect("header line"),
        )
        .expect("header json");
        let header_only = crate::persist::io::encode_materialization(
            format!("{header_json}\n").as_bytes(),
            &[],
            backend.compression(),
        )
        .expect("encode");
        fs::write(&path, header_only).expect("shrink");
        let error = handle.read(0, ALL).expect_err("shrunk log refuses");
        assert!(
            error.message().starts_with(&format!(
                "session \"{}\": stored log shrank below a previously observed prefix (0 < 2)",
                handle.id().as_str()
            )),
            "verbatim shrink refusal: {}",
            error.message()
        );
        handle.close().expect("close");
    }

    #[test]
    fn torn_tail_repaired_on_first_append() {
        let (root, backend) = backend("torn");
        let handle = backend
            .create(&header(&format!("s-t-{}", unique_id())), 0)
            .expect("create");
        handle.append(&turn(0, 1)).expect("append");
        handle.flush().expect("flush");
        handle.close().expect("close");
        let path = filename::log_path(root.path(), None, handle.id(), backend.compression())
            .expect("path");
        // A crash mid-append leaves one incomplete record behind.
        // A crash mid-append leaves one incomplete frame behind: the magic
        // lands, the payload does not (dsh zstd torn-tail shape).
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        std::io::Write::write_all(&mut file, &[0x28, 0xB5, 0x2F, 0xFD, 0x01, 0x00])
            .expect("torn frame");
        drop(file);
        let reopened = backend
            .open(handle.id(), SessionAccess::Write)
            .expect("write open with torn tail");
        // The torn tail is never returned to reads.
        assert_eq!(reopened.read(0, ALL).expect("read").events.len(), 2);
        // The first append repairs: truncate torn bytes, then write.
        reopened.append(&turn(2, 2)).expect("append repairs");
        reopened.flush().expect("flush");
        let events = reopened.read(0, ALL).expect("read").events;
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].seq.get(), 0);
        assert_eq!(events[3].seq.get(), 3);
        reopened.close().expect("close");
    }

    #[test]
    fn live_routing_drains_after_the_window() {
        let (root, backend) = backend("live");
        let _keep = &root;
        let handle = backend
            .create(&header(&format!("s-live-{}", unique_id())), 0)
            .expect("create");
        for routed in turn(0, 1) {
            handle.enqueue_live(&routed);
        }
        // The 30 ms window fires; the routed batch persists without any
        // explicit append.
        std::thread::sleep(Duration::from_millis(150));
        let events = handle.read(0, ALL).expect("read after window").events;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq.get(), 0);
        handle.close().expect("close");
    }

    #[test]
    fn live_drain_failure_pauses_then_close_drains() {
        let (root, backend) = backend("paused");
        let handle = backend
            .create(&header(&format!("s-pause-{}", unique_id())), 0)
            .expect("create");
        handle.append(&turn(0, 1)).expect("append");
        handle.flush().expect("flush");
        let path = filename::log_path(root.path(), None, handle.id(), backend.compression())
            .expect("path");
        // Make the artifact unwritable so the routed drain fails loudly.
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o444);
        fs::set_permissions(&path, perms.clone()).expect("chmod");
        handle.enqueue_live(&turn(2, 2)[0]);
        // The window fires and the drain fails; the batch stays buffered.
        std::thread::sleep(Duration::from_millis(150));
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms).expect("chmod back");
        // Close drains until a full pass leaves the buffer empty.
        let id = handle.id().clone();
        handle.close().expect("close drains the retained batch");
        let reader = backend.open(&id, SessionAccess::Read).expect("read open");
        let events = reader.read(0, ALL).expect("read").events;
        assert_eq!(events.len(), 3);
        reader.close().expect("close");
    }

    #[test]
    fn version_gate_refuses_foreign_generations_verbatim() {
        let (root, backend) = backend("gate");
        let id_text = format!("s-gate-{}", unique_id());
        let id = SessionId::new(id_text.clone());
        let dir = root
            .path()
            .join("proj-gate")
            .join(paths::encode_segment(&id_text).expect("encode"));
        fs::create_dir_all(&dir).expect("dirs");
        for version in [3_u64, 5] {
            let header_line = format!(
                "{{\"type\":\"session\",\"version\":{version},\"id\":\"{id_text}\",\"createdAt\":1,\"isSeeded\":false,\"delegationDepth\":0}}\n"
            );
            let path = dir.join(format!("session.v{version}.jsonl.zstd"));
            let frame =
                crate::persist::zstd::compress_zstd_frame(header_line.as_bytes()).expect("frame");
            fs::write(&path, frame).expect("write artifact");
            for access in [SessionAccess::Read, SessionAccess::Write] {
                let error = backend
                    .open(&id, access)
                    .expect_err("foreign generation refuses");
                let expected = if version > SESSION_FORMAT_VERSION {
                    format!(
                        "session \"{id_text}\" uses log format v{version}, but this harness reads \
only v4: the log was written by a newer harness — upgrade the harness to open it (raw log: {})",
                        path.display()
                    )
                } else {
                    format!(
                        "session \"{id_text}\" uses log format v{version}, older than the \
supported v4, and this build ships no upgrade path for it (raw log: {})",
                        path.display()
                    )
                };
                assert_eq!(error.message(), expected, "open {access:?} v{version}");
            }
            let stat_error = backend
                .stat(&id)
                .expect_err("stat refuses foreign generation");
            assert!(
                stat_error
                    .message()
                    .contains("upgrade the harness to open it")
                    || stat_error.message().contains("no upgrade path")
            );
        }
    }

    #[test]
    fn legacy_flat_artifact_refuses_verbatim() {
        let (root, backend) = backend("legacy");
        let id_text = format!("s-flat-{}", unique_id());
        let id = SessionId::new(id_text.clone());
        let project = root.path().join("legacy-proj");
        fs::create_dir_all(&project).expect("project dir");
        let flat = project.join(format!(
            "{}.jsonl",
            paths::encode_segment(&id_text).expect("encode")
        ));
        fs::write(&flat, "garbage").expect("flat artifact");
        let error = backend
            .open(&id, SessionAccess::Read)
            .expect_err("flat layout refuses");
        assert_eq!(
            error.message(),
            format!(
                "session artifact \"{}\" uses the unsupported flat-file layout; use a separate \
root or move it into a project/session directory before loading",
                flat.display()
            )
        );
        let list_error = backend.list().expect_err("legacy layout fails the list");
        assert_eq!(
            list_error.message(),
            error.message(),
            "list and open report the same legacy refusal"
        );
    }

    #[test]
    fn ten_concurrent_writers_persist_serially_per_session() {
        let (root, backend) = backend("concurrent");
        let _keep = &root;
        let handles: Vec<JsonlSessionHandle> = (0..10)
            .map(|index| {
                backend
                    .create(&header(&format!("s-par-{index}-{}", unique_id())), 0)
                    .expect("create")
            })
            .collect();
        std::thread::scope(|scope| {
            for (index, handle) in handles.iter().enumerate() {
                scope.spawn(move || {
                    for round in 0..5u64 {
                        let batch = vec![
                            event(
                                round * 2,
                                SessionEventData::TurnStart(TurnStartData { turn: round + 1 }),
                            ),
                            event(
                                round * 2 + 1,
                                SessionEventData::TurnEnd(TurnEndData {
                                    turn: round + 1,
                                    reason: TurnEndReason::Known(TurnEndReasonKind::Completed),
                                }),
                            ),
                        ];
                        handle.append(&batch).expect("contended append");
                    }
                    handle.flush().expect("contended flush");
                    let _ = index;
                });
            }
        });
        for handle in &handles {
            let events = handle.read(0, ALL).expect("read own log").events;
            assert_eq!(events.len(), 10, "per-session log complete");
            for (position, persisted) in events.iter().enumerate() {
                assert_eq!(persisted.seq.get(), position as u64, "seq dense");
            }
            handle.close().expect("close");
        }
    }

    #[test]
    fn bounded_parallel_list_serves_a_large_store() {
        let (root, backend) = backend("bigstore");
        let _keep = &root;
        for index in 0..40u64 {
            let handle = backend
                .create(&header(&format!("s-big-{index}-{}", unique_id())), 0)
                .expect("create");
            handle.append(&turn(0, 1)).expect("append");
            handle.flush().expect("flush");
            handle.close().expect("close");
        }
        // One broken artifact skips its entry instead of failing the scan.
        let broken_dir = root
            .path()
            .join("_no-cwd")
            .join(paths::encode_segment("s-broken").expect("encode"));
        fs::create_dir_all(&broken_dir).expect("dir");
        let broken =
            crate::persist::zstd::compress_zstd_frame(b"{\"nope\":1}\n".as_slice()).expect("frame");
        fs::write(broken_dir.join("session.v4.jsonl.zstd"), broken).expect("broken artifact");
        let listed = backend.list().expect("large-store list");
        assert_eq!(listed.len(), 40, "broken header skipped");
        assert!(listed.iter().all(|snap| snap.size_bytes.is_some()));
    }

    #[test]
    fn append_resolution_is_fresh_for_later_opens() {
        let (root, backend) = backend("fresh");
        let _keep = &root;
        let writer = backend
            .create(&header(&format!("s-fresh-{}", unique_id())), 0)
            .expect("create");
        writer.append(&turn(0, 1)).expect("append");
        // The freshness promise: a read STARTED after the append resolves
        // observes at least that prefix, on any handle.
        let reader = backend
            .open(writer.id(), SessionAccess::Read)
            .expect("open");
        assert_eq!(reader.read(0, ALL).expect("read").events.len(), 2);
        writer.flush().expect("flush");
        writer.close().expect("close");
        reader.close().expect("close");
    }

    #[test]
    fn close_reports_the_drain_failure_and_stays_closed() {
        let (root, backend) = backend("failclose");
        let handle = backend
            .create(&header(&format!("s-fc-{}", unique_id())), 0)
            .expect("create");
        handle.append(&turn(0, 1)).expect("append");
        handle.flush().expect("flush");
        let path = filename::log_path(root.path(), None, handle.id(), backend.compression())
            .expect("path");
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        perms.set_mode(0o444);
        fs::set_permissions(&path, perms.clone()).expect("chmod");
        handle.enqueue_live(&turn(2, 2)[0]);
        std::thread::sleep(Duration::from_millis(150));
        // The close drains first; the drain failure is THE close outcome.
        let outcome = handle.close().expect_err("failed drain fails the close");
        assert!(
            matches!(
                outcome,
                PersistError::Io(ref error)
                    if error.kind() == std::io::ErrorKind::PermissionDenied
            ),
            "drain failure surfaces: {outcome}"
        );
        // The handle stays closed: the second close shares the outcome.
        let again = handle.close().expect_err("idempotent outcome");
        assert_eq!(again.message(), outcome.message());
        perms.set_mode(0o600);
        fs::set_permissions(&path, perms).expect("chmod back");
    }
}
