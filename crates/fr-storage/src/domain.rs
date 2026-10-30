//! The domain runtime: authoritative in-memory state, the single
//! per-domain FIFO write chain, and change emission (the dsh
//! `storage-domain` `domain.ts`).
//!
//! Reads are synchronous from memory; every write queues on the chain,
//! completes backend durability FIRST, then mutates memory, then emits
//! `domain/changed` — a rejected backend write leaves memory untouched
//! (no divergence between reads and the medium), and events carry values
//! that equal the in-memory state at emission, in write order.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::Value;

use crate::error::{FrStorageError, FrStorageErrorCode};
use crate::unit::SingleJsonUnit;

/// The unit name pattern (`^[a-z][a-z0-9_]*$`): the dsh `UNIT_NAME_RE`,
/// enforced on the domain name and every table name at declaration time.
#[must_use]
pub fn unit_name_ok(name: &str) -> bool {
    crate::format::unit_name_ok(name)
}

/// Static declaration of one domain: identity and record layout (the dsh
/// `DomainSpec`; record schemas stay with the owning consumer package, so
/// the spec carries only the medium shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainSpec {
    /// Domain name; must match the unit name pattern.
    pub name: String,
    /// Current domain format version (non-negative integer).
    pub version: u32,
    /// Declared table names; each must match the unit name pattern.
    pub tables: Vec<String>,
    /// Whether the domain declares a global singleton slot.
    pub has_global: bool,
}

/// Declares one domain, validating the spec fields fail-loud at the
/// owning package's setup.
///
/// # Errors
///
/// Rejects a name or table outside the unit name pattern.
pub fn define_domain(spec: DomainSpec) -> Result<DomainSpec, FrStorageError> {
    if !unit_name_ok(&spec.name) {
        return Err(FrStorageError::malformed_medium(format!(
            "domain name '{}' must match ^[a-z][a-z0-9_]*$",
            spec.name
        )));
    }
    for table in &spec.tables {
        if !unit_name_ok(table) {
            return Err(FrStorageError::malformed_medium(format!(
                "domain '{}' table name '{table}' must match ^[a-z][a-z0-9_]*$",
                spec.name
            )));
        }
    }
    Ok(spec)
}

/// One durable change notification (the dsh `DomainChanged`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainChanged {
    /// Domain the change landed in.
    pub domain: String,
    /// Table the change landed in.
    pub table: String,
    /// Record key.
    pub key: String,
    /// Whether the record was written or removed.
    pub operation: DomainOperation,
    /// The new record value for a put.
    pub value: Option<Value>,
}

/// Whether a change put or removed its record (the dsh operation union).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainOperation {
    /// The record was inserted or replaced.
    Put,
    /// The record was removed.
    Deleted,
}

/// The emission face installed at construction (the plugin wires it to
/// `HostExports.emit`; tests capture directly).
pub type EmitFace = Arc<dyn Fn(&DomainChanged) + Send + Sync>;

/// The facility hook run once after teardown completes (the dsh
/// `onClosed` shape: the name frees up for reopening only once the
/// domain is fully closed).
pub type OnClose = Arc<dyn Fn() + Send + Sync>;

impl DomainOperation {
    /// The stable wire-facing discriminant (the dsh operation strings).
    #[must_use]
    pub const fn as_debug_name(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::Deleted => "deleted",
        }
    }
}

/// The in-memory authoritative state behind the read lock.
#[derive(Debug, Default)]
struct MemoryState {
    tables: HashMap<String, HashMap<String, Value>>,
    global: Option<Value>,
}

/// One write-chain job (the dsh promise-chain link).
enum Job {
    /// Upserts one record on the chain.
    Put {
        table: String,
        key: String,
        value: Value,
        reply: Sender<Result<(), FrStorageError>>,
    },
    /// Deletes one record on the chain; existence decided at the slot.
    Delete {
        table: String,
        key: String,
        reply: Sender<Result<bool, FrStorageError>>,
    },
    /// Writes the global singleton on the chain.
    SetGlobal {
        value: Value,
        reply: Sender<Result<(), FrStorageError>>,
    },
    /// Drains every queued job, then closes the unit.
    Close { reply: Sender<()> },
}

/// The shared runtime one [`Domain`] handle reaches through.
struct DomainShared {
    spec_name: String,
    memory: Mutex<MemoryState>,
    unit: SingleJsonUnit,
    emit: EmitFace,
    job_tx: Mutex<Sender<Job>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// Set synchronously by `close` before the close job is queued: new
    /// writes reject while the drain proceeds.
    disposing: AtomicBool,
    /// Set by the worker after the close job: reads reject from here.
    closed: AtomicBool,
    /// The facility hook the close job runs once teardown completes.
    on_close: Mutex<Option<OnClose>>,
}

/// Locks the memory state; a poisoned lock is a hard medium failure.
fn lock_memory(
    memory: &Mutex<MemoryState>,
) -> Result<std::sync::MutexGuard<'_, MemoryState>, FrStorageError> {
    memory.lock().map_err(|error| {
        FrStorageError::malformed_medium(format!("domain memory state lock poisoned: {error}"))
    })
}

/// Runs the write-chain worker: executes jobs FIFO against the unit
/// (durability first), mutates memory, and emits changes in write order.
fn run_worker(shared: &Arc<DomainShared>, rx: &mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Put {
                table,
                key,
                value,
                reply,
            } => {
                let outcome = chain_put(shared, &table, &key, value);
                let _ = reply.send(outcome);
            }
            Job::Delete { table, key, reply } => {
                let outcome = chain_delete(shared, &table, &key);
                let _ = reply.send(outcome);
            }
            Job::SetGlobal { value, reply } => {
                let outcome = chain_set_global(shared, value);
                {
                    let _ = reply.send(outcome);
                }
            }
            Job::Close { reply } => {
                shared.unit.close();
                shared.closed.store(true, Ordering::SeqCst);
                if let Ok(mut hook) = shared.on_close.lock()
                    && let Some(on_close) = hook.take()
                {
                    on_close();
                }
                let _ = reply.send(());
                return;
            }
        }
    }
}

/// Chain slot for one put: unit durability first, memory second, emit
/// third.
fn chain_put(
    shared: &Arc<DomainShared>,
    table: &str,
    key: &str,
    value: Value,
) -> Result<(), FrStorageError> {
    shared.unit.put_record(table, key, value.clone())?;
    let mut memory = lock_memory(&shared.memory)?;
    if let Some(records) = memory.tables.get_mut(table) {
        records.insert(key.to_owned(), value.clone());
    }
    drop(memory);
    (shared.emit)(&DomainChanged {
        domain: shared.spec_name.clone(),
        table: table.to_owned(),
        key: key.to_owned(),
        operation: DomainOperation::Put,
        value: Some(value),
    });
    Ok(())
}

/// Chain slot for one delete: existence is decided at this job's chain
/// slot, and an absent key writes nothing and emits nothing.
fn chain_delete(
    shared: &Arc<DomainShared>,
    table: &str,
    key: &str,
) -> Result<bool, FrStorageError> {
    {
        let memory = lock_memory(&shared.memory)?;
        let absent = !memory
            .tables
            .get(table)
            .is_some_and(|records| records.contains_key(key));
        drop(memory);
        if absent {
            return Ok(false);
        }
    }
    shared.unit.delete_record(table, key)?;
    let mut memory = lock_memory(&shared.memory)?;
    if let Some(records) = memory.tables.get_mut(table) {
        records.remove(key);
    }
    drop(memory);
    (shared.emit)(&DomainChanged {
        domain: shared.spec_name.clone(),
        table: table.to_owned(),
        key: key.to_owned(),
        operation: DomainOperation::Deleted,
        value: None,
    });
    Ok(true)
}

/// Chain slot for one global set: durability, memory, emit.
fn chain_set_global(shared: &Arc<DomainShared>, value: Value) -> Result<(), FrStorageError> {
    shared.unit.set_global(value.clone())?;
    let mut memory = lock_memory(&shared.memory)?;
    memory.global = Some(value.clone());
    drop(memory);
    (shared.emit)(&DomainChanged {
        domain: shared.spec_name.clone(),
        table: String::new(),
        key: String::new(),
        operation: DomainOperation::Put,
        value: Some(value),
    });
    Ok(())
}

/// One table's key-record snapshot pairs.
pub type TableSnapshot = Vec<(String, Value)>;
/// Every declared table's snapshot in declaration order.
pub type DomainSnapshot = Vec<(String, TableSnapshot)>;

/// The poisoned-lock diagnostic shared by the domain lock sites.
fn poisoned_domain(error: impl std::fmt::Display) -> FrStorageError {
    FrStorageError::malformed_medium(format!("domain state lock poisoned: {error}"))
}

/// One open domain (the dsh `Domain` handle): reads from memory, writes
/// through the FIFO chain, one worker thread per domain joined at close.
#[derive(Clone)]
pub struct Domain {
    shared: Arc<DomainShared>,
}

impl std::fmt::Debug for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Domain")
            .field("name", &self.shared.spec_name)
            .finish_non_exhaustive()
    }
}

impl Domain {
    /// Opens the domain: constructs the runtime over one opened unit,
    /// loads the validated snapshot into memory, and starts the
    /// write-chain worker. `records` carries the snapshot's per-table
    /// records, already validated by the caller (the owner-package
    /// schemas run at the consumer's open protocol).
    ///
    /// # Errors
    ///
    /// Rejects a snapshot table the spec does not declare, and a worker
    /// thread that failed to spawn.
    pub fn open(
        spec: &DomainSpec,
        unit: SingleJsonUnit,
        records: HashMap<String, HashMap<String, Value>>,
        emit: EmitFace,
        on_close: Option<OnClose>,
    ) -> Result<Self, FrStorageError> {
        let mut memory = MemoryState::default();
        for table in &spec.tables {
            memory.tables.insert(table.clone(), HashMap::new());
        }
        for (table, table_records) in records {
            let Some(slot) = memory.tables.get_mut(&table) else {
                return Err(FrStorageError::new(
                    FrStorageErrorCode::InvalidRecord,
                    format!(
                        "domain '{}' snapshot carries undeclared table '{table}'",
                        spec.name
                    ),
                ));
            };
            *slot = table_records;
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let shared = Arc::new(DomainShared {
            spec_name: spec.name.clone(),
            memory: Mutex::new(memory),
            unit,
            emit,
            job_tx: Mutex::new(tx),
            worker: Mutex::new(None),
            disposing: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            on_close: Mutex::new(on_close),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name(format!("fr-storage-domain-{}", spec.name))
            .spawn(move || run_worker(&worker_shared, &rx))
            .map_err(|error| {
                FrStorageError::malformed_medium(format!(
                    "domain '{}' worker thread failed to spawn: {error}",
                    spec.name
                ))
            })?;
        match shared.worker.lock() {
            Ok(mut slot) => *slot = Some(worker),
            Err(error) => return Err(poisoned_domain(error)),
        }
        Ok(Self { shared })
    }

    /// The domain name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.shared.spec_name
    }

    /// Resolves one declared table handle; handles are cheap views and
    /// repeated calls are equivalent.
    ///
    /// # Errors
    ///
    /// Rejects an undeclared table name.
    pub fn table(&self, name: &str) -> Result<KvTable, FrStorageError> {
        let declared = self
            .shared
            .unit
            .descriptor()
            .tables
            .iter()
            .any(|table| table == name);
        if !declared {
            return Err(FrStorageError::new(
                FrStorageErrorCode::MissingKey,
                format!(
                    "domain '{}' declares no table '{name}'",
                    self.shared.spec_name
                ),
            ));
        }
        Ok(KvTable {
            domain: self.clone(),
            table: name.to_owned(),
        })
    }

    /// Reads one record synchronously from memory.
    ///
    /// # Errors
    ///
    /// Rejects a closed domain.
    pub fn get(&self, table: &str, key: &str) -> Result<Option<Value>, FrStorageError> {
        self.assert_readable()?;
        let memory = lock_memory(&self.shared.memory)?;
        Ok(memory
            .tables
            .get(table)
            .and_then(|records| records.get(key))
            .cloned())
    }

    /// Snapshots one table's `[key, record]` pairs in insertion order.
    ///
    /// # Errors
    ///
    /// Rejects a closed domain.
    pub fn entries(&self, table: &str) -> Result<Vec<(String, Value)>, FrStorageError> {
        self.assert_readable()?;
        let memory = lock_memory(&self.shared.memory)?;
        Ok(memory
            .tables
            .get(table)
            .map(|records| {
                records
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Snapshots the table's key list.
    ///
    /// # Errors
    ///
    /// Rejects a closed domain.
    pub fn keys(&self, table: &str) -> Result<Vec<String>, FrStorageError> {
        Ok(self
            .entries(table)?
            .into_iter()
            .map(|(key, _)| key)
            .collect())
    }

    /// The table's current record count.
    ///
    /// # Errors
    ///
    /// Rejects a closed domain.
    pub fn size(&self, table: &str) -> Result<usize, FrStorageError> {
        Ok(self.entries(table)?.len())
    }

    /// Snapshots every declared table in declaration order (the dsh
    /// `load_all` table projection).
    ///
    /// # Errors
    ///
    /// Rejects a closed domain.
    pub fn snapshot_tables(&self) -> Result<DomainSnapshot, FrStorageError> {
        self.assert_readable()?;
        let memory = lock_memory(&self.shared.memory)?;
        Ok(self
            .shared
            .unit
            .descriptor()
            .tables
            .iter()
            .map(|table| {
                let records = memory
                    .tables
                    .get(table)
                    .map(|slot| slot.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();
                (table.clone(), records)
            })
            .collect())
    }

    /// Reads the global singleton; the never-written slot serves `None`
    /// (the dsh `null` sentinel; the M1 spec vocabulary declares no
    /// configured initial).
    ///
    /// # Errors
    ///
    /// Rejects a closed domain and a domain that declares no global slot.
    pub fn global_get(&self) -> Result<Option<Value>, FrStorageError> {
        self.assert_readable()?;
        self.require_global_slot()?;
        let memory = lock_memory(&self.shared.memory)?;
        Ok(memory.global.clone())
    }

    /// Replaces the global singleton durably (the first `set`
    /// materializes it on the medium).
    ///
    /// # Errors
    ///
    /// Rejects a closed or draining domain, a domain that declares no
    /// global slot, and a failed durable write.
    pub fn global_set(&self, value: Value) -> Result<(), FrStorageError> {
        self.reject_writes()?;
        self.require_global_slot()?;
        let (tx, rx) = mpsc::channel();
        self.send_job(Job::SetGlobal { value, reply: tx })?;
        rx.recv()
            .map_err(|error| FrStorageError::closed(format!("domain worker exited: {error}")))?
    }

    /// Closes the domain: rejects new writes immediately, drains queued
    /// writes through the close job's FIFO slot, closes the unit, then
    /// rejects reads. Idempotent.
    pub fn close(&self) {
        if self.shared.disposing.swap(true, Ordering::SeqCst) {
            self.wait_worker_exit();
            return;
        }
        let (tx, rx) = mpsc::channel::<()>();
        if self.send_job(Job::Close { reply: tx }).is_err() {
            self.shared.closed.store(true, Ordering::SeqCst);
            self.wait_worker_exit();
            return;
        }
        let _ = rx.recv();
        self.wait_worker_exit();
    }

    /// Sends one job to the worker (the write-chain enqueue).
    fn send_job(&self, job: Job) -> Result<(), FrStorageError> {
        let tx = self.shared.job_tx.lock().map_err(poisoned_domain)?;
        tx.send(job)
            .map_err(|error| FrStorageError::closed(format!("domain worker exited: {error}")))
    }

    /// Waits for the worker thread to exit and collects the handle.
    fn wait_worker_exit(&self) {
        let handle = self
            .shared
            .worker
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Rejects reads after closure (draining reads stay valid).
    fn assert_readable(&self) -> Result<(), FrStorageError> {
        if self.shared.closed.load(Ordering::SeqCst) {
            return Err(self.closed_error());
        }
        Ok(())
    }

    /// Rejects writes once close has begun.
    fn reject_writes(&self) -> Result<(), FrStorageError> {
        if self.shared.disposing.load(Ordering::SeqCst) {
            return Err(self.closed_error());
        }
        Ok(())
    }

    /// The stable `closed` rejection for this domain.
    fn closed_error(&self) -> FrStorageError {
        FrStorageError::closed(format!("domain '{}' is closed", self.shared.spec_name))
    }

    /// Rejects a domain that declares no global slot.
    fn require_global_slot(&self) -> Result<(), FrStorageError> {
        if !self.shared.unit.declared_global() {
            return Err(FrStorageError::new(
                FrStorageErrorCode::MissingKey,
                format!("domain '{}' declares no global", self.shared.spec_name),
            ));
        }
        Ok(())
    }
}

/// A table handle bound to its domain's write machinery (the dsh
/// `KvTable`; a cheap view — repeated resolution is equivalent).
#[derive(Debug, Clone)]
pub struct KvTable {
    domain: Domain,
    table: String,
}

impl KvTable {
    /// Reads one record synchronously from memory.
    ///
    /// # Errors
    /// Rejects a closed domain.
    pub fn get(&self, key: &str) -> Result<Option<Value>, FrStorageError> {
        self.domain.get(&self.table, key)
    }

    /// Snapshots the table's `[key, record]` pairs in insertion order.
    ///
    /// # Errors
    /// Rejects a closed domain.
    pub fn entries(&self) -> Result<Vec<(String, Value)>, FrStorageError> {
        self.domain.entries(&self.table)
    }

    /// Snapshots the table's keys.
    ///
    /// # Errors
    /// Rejects a closed domain.
    pub fn keys(&self) -> Result<Vec<String>, FrStorageError> {
        self.domain.keys(&self.table)
    }

    /// The record count.
    ///
    /// # Errors
    /// Rejects a closed domain.
    pub fn size(&self) -> Result<usize, FrStorageError> {
        self.domain.size(&self.table)
    }

    /// Upserts one record durably (full record, no partial merge).
    ///
    /// # Errors
    /// Rejects a closed or draining domain and a failed durable write.
    pub fn put(&self, key: &str, value: Value) -> Result<(), FrStorageError> {
        self.domain.reject_writes()?;
        let (tx, rx) = mpsc::channel();
        self.domain.send_job(Job::Put {
            table: self.table.clone(),
            key: key.to_owned(),
            value,
            reply: tx,
        })?;
        rx.recv()
            .map_err(|error| FrStorageError::closed(format!("domain worker exited: {error}")))?
    }

    /// Deletes one record durably; an absent key is a no-op returning
    /// `false` with no write and no event.
    ///
    /// # Errors
    /// Rejects a closed or draining domain and a failed durable write.
    pub fn delete(&self, key: &str) -> Result<bool, FrStorageError> {
        self.domain.reject_writes()?;
        let (tx, rx) = mpsc::channel();
        self.domain.send_job(Job::Delete {
            table: self.table.clone(),
            key: key.to_owned(),
            reply: tx,
        })?;
        rx.recv()
            .map_err(|error| FrStorageError::closed(format!("domain worker exited: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unit::UnitDescriptor;
    use serde_json::json;
    use std::path::PathBuf;

    fn spec(name: &str) -> DomainSpec {
        DomainSpec {
            name: name.to_owned(),
            version: 1,
            tables: vec!["tasks".to_owned()],
            has_global: false,
        }
    }

    /// Opens one domain over a scratch root with a captured event log.
    fn opened(tag: &str) -> (Domain, PathBuf, Arc<Mutex<Vec<String>>>) {
        let root =
            std::env::temp_dir().join(format!("fr-storage-domain-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let unit = SingleJsonUnit::open(
            UnitDescriptor {
                name: "schedule".to_owned(),
                version: 1,
                tables: vec!["tasks".to_owned()],
                has_global: false,
            },
            &root,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let emit: EmitFace = Arc::new(move |changed: &DomainChanged| {
            sink.lock()
                .unwrap_or_else(|_| panic!("event log poisoned"))
                .push(format!(
                    "{}:{}:{}",
                    changed.table,
                    changed.key,
                    changed.operation.as_debug_name()
                ));
        });
        let domain = Domain::open(&spec("schedule"), unit, HashMap::new(), emit, None)
            .unwrap_or_else(|e| panic!("{e}"));
        (domain, root, events)
    }

    #[test]
    fn define_domain_rejects_foreign_names() {
        let _valid = define_domain(spec("schedule")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            define_domain(spec("Schedule")).unwrap_err().code,
            FrStorageErrorCode::MalformedMedium
        );
        let mut bad = spec("ok");
        bad.tables = vec!["no-dash".to_owned()];
        assert_eq!(
            define_domain(bad).unwrap_err().code,
            FrStorageErrorCode::MalformedMedium
        );
    }

    #[test]
    fn writes_chain_fifo_and_events_match_write_order() {
        let (domain, root, events) = opened("fifo");
        let table = domain.table("tasks").unwrap_or_else(|e| panic!("{e}"));
        for index in 0..8 {
            table
                .put(&format!("k{index}"), json!(index))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(table.size().unwrap_or_else(|e| panic!("{e}")), 8);
        let keys: Vec<String> = {
            let log = events.lock().unwrap_or_else(|_| panic!("poisoned"));
            log.iter()
                .map(|entry| entry.split(':').nth(1).unwrap_or_default().to_owned())
                .collect()
        };
        let expected: Vec<String> = (0..8).map(|index| format!("k{index}")).collect();
        assert_eq!(keys, expected);
        domain.close();
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn delete_absent_is_false_with_no_event() {
        let (domain, root, events) = opened("delete-absent");
        let table = domain.table("tasks").unwrap_or_else(|e| panic!("{e}"));
        let deleted = table.delete("never").unwrap_or_else(|e| panic!("{e}"));
        assert!(!deleted);
        assert!(
            events
                .lock()
                .unwrap_or_else(|_| panic!("poisoned"))
                .is_empty()
        );
        table
            .put("gone", json!(1))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(table.delete("gone").unwrap_or_else(|e| panic!("{e}")));
        assert_eq!(
            events.lock().unwrap_or_else(|_| panic!("poisoned")).len(),
            2
        );
        domain.close();
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn close_rejects_writes_then_reads_and_is_idempotent() {
        let (domain, root, _events) = opened("close");
        domain.close();
        domain.close();
        let table = domain.table("tasks").unwrap_or_else(|e| panic!("{e}"));
        let error = table.put("late", json!(1)).unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::Closed);
        let error = domain.get("tasks", "late").unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::Closed);
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn undeclared_tables_and_global_slots_reject() {
        let (domain, root, _events) = opened("undeclared");
        let error = domain.table("other").unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::MissingKey);
        let error = domain.global_get().unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::MissingKey);
        domain.close();
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn a_global_slot_round_trips_when_declared() {
        let root =
            std::env::temp_dir().join(format!("fr-storage-domain-global-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let unit = SingleJsonUnit::open(
            UnitDescriptor {
                name: "schedule".to_owned(),
                version: 1,
                tables: vec![],
                has_global: true,
            },
            &root,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let domain = Domain::open(
            &DomainSpec {
                name: "schedule".to_owned(),
                version: 1,
                tables: vec![],
                has_global: true,
            },
            unit,
            HashMap::new(),
            Arc::new(|_| {}),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(domain.global_get().unwrap_or_else(|e| panic!("{e}")), None);
        domain
            .global_set(json!({"v": 3}))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            domain.global_get().unwrap_or_else(|e| panic!("{e}")),
            Some(json!({"v": 3}))
        );
        domain.close();
        let reopened = SingleJsonUnit::open(
            UnitDescriptor {
                name: "schedule".to_owned(),
                version: 1,
                tables: vec![],
                has_global: true,
            },
            &root,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (_, global) = reopened.load_all().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(global, Some(json!({"v": 3})));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }
}
