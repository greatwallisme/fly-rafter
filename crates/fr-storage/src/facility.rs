//! The domain facility: opens declared domains over the JSON unit
//! backend, enforces single-open per domain name, and closes leftovers
//! on teardown (the dsh `DomainFacility`).
//!
//! The backend route collapses because fr mounts one built-in backend.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::domain::{Domain, DomainSpec, EmitFace, OnClose};
use crate::error::{FrStorageError, FrStorageErrorCode};
use crate::unit::{SingleJsonUnit, UnitDescriptor};

/// The opened-domain facility (the dsh `DomainFacility`). One instance
/// owns the open-domain table and enforces single-open per domain name.
#[derive(Default)]
pub struct DomainFacility {
    domains: Mutex<HashMap<String, Domain>>,
    /// Names reserved by an in-flight or completed open, so concurrent
    /// opens of one name fail loud.
    reserved: Mutex<HashSet<String>>,
    /// Prepared opens awaiting the consumer's validation commit or
    /// refusal (the two-phase open protocol's stash).
    pending: Mutex<HashMap<String, PreparedDomain>>,
    /// Backend root directory the unit files live under.
    root: Mutex<Option<PathBuf>>,
    /// The change emission face handed to every opened domain.
    emit: Mutex<Option<EmitFace>>,
}

impl DomainFacility {
    /// Creates one empty facility; [`DomainFacility::mount`] installs
    /// the root and the emission face.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pops one prepared open by name (the wire protocol's second-phase
    /// retrieval; the first phase stashed it in [`Self::prepare`]).
    ///
    /// # Errors
    ///
    /// Rejects a name with no prepared open.
    pub fn take_prepared(&self, name: &str) -> Result<PreparedDomain, FrStorageError> {
        let mut pending = self.pending.lock().map_err(locked_facility)?;
        pending.remove(name).ok_or_else(|| {
            FrStorageError::new(
                FrStorageErrorCode::MissingKey,
                format!("domain '{name}' has no prepared open to complete"),
            )
        })
    }

    /// Mounts the backend root and the change emission face.
    ///
    /// # Errors
    ///
    /// Rejects a mount over an existing one (the dsh storage hub's
    /// `duplicate-mount` shape).
    pub fn mount(&self, root: PathBuf, emit: EmitFace) -> Result<(), FrStorageError> {
        let mut slot = self.root.lock().map_err(locked_facility)?;
        if slot.is_some() {
            return Err(FrStorageError::new(
                FrStorageErrorCode::AlreadyOpen,
                "storage form 'domain' is already mounted",
            ));
        }
        *slot = Some(root);
        drop(slot);
        let mut face = self.emit.lock().map_err(|error| locked_facility(error))?;
        *face = Some(emit);
        drop(face);
        Ok(())
    }

    /// Opens one declared domain: reserves the name, opens the unit,
    /// and hands the loaded snapshot to the caller for validation via
    /// [`Domain::open`]. The returned snapshot is the raw stored state —
    /// the consumer validates records against its own schemas and
    /// refuses the open on the first invalid record.
    ///
    /// # Errors
    ///
    /// Rejects an already-open or reserved name, a facility without a
    /// mount, an invalid spec, and every backend open failure
    /// (`malformed-medium`/`version-mismatch`).
    pub fn prepare(self: &Arc<Self>, spec: DomainSpec) -> Result<SnapshotPayload, FrStorageError> {
        let spec = crate::domain::define_domain(spec)?;
        let name = spec.name.clone();
        {
            let mut reserved = self.reserved.lock().map_err(locked_facility)?;
            // A committed open domain holds the name just like a
            // reservation does.
            let open = self
                .domains
                .lock()
                .map_or(true, |domains| domains.contains_key(&name));
            if open || reserved.contains(&name) {
                return Err(FrStorageError::new(
                    FrStorageErrorCode::AlreadyOpen,
                    format!("domain '{name}' is already open"),
                ));
            }
            reserved.insert(name.clone());
        }
        match Self::prepare_locked(Arc::clone(self), spec) {
            Ok((prepared, snapshot)) => {
                if let Ok(mut slot) = self.pending.lock() {
                    slot.insert(name, prepared);
                }
                Ok(snapshot)
            }
            Err(error) => {
                let _ = self.release(&name);
                Err(error)
            }
        }
    }

    /// The mount-rooted open path behind [`DomainFacility::prepare`].
    fn prepare_locked(
        facility: Arc<Self>,
        spec: DomainSpec,
    ) -> Result<(PreparedDomain, SnapshotPayload), FrStorageError> {
        let root = {
            let slot = facility.root.lock().map_err(locked_facility)?;
            slot.clone().ok_or_else(|| {
                FrStorageError::new(
                    FrStorageErrorCode::Closed,
                    "storage form 'domain' is not mounted",
                )
            })?
        };
        let descriptor = UnitDescriptor {
            name: spec.name.clone(),
            version: spec.version,
            tables: spec.tables.clone(),
            has_global: spec.has_global,
        };
        let unit = SingleJsonUnit::open(descriptor, &root)?;
        let (tables, global) = unit.load_all()?;
        let mut records = HashMap::new();
        for (table, table_records) in tables {
            let records_map = table_records
                .as_object()
                .map(|map| {
                    map.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect::<HashMap<String, Value>>()
                })
                .unwrap_or_default();
            records.insert(table, records_map);
        }
        let emit = {
            let face = facility
                .emit
                .lock()
                .map_err(|error| locked_facility(error))?;
            face.clone().ok_or_else(|| {
                FrStorageError::new(
                    FrStorageErrorCode::Closed,
                    "storage form 'domain' is not mounted",
                )
            })?
        };
        let snapshot = SnapshotPayload {
            name: spec.name.clone(),
            records: records.clone(),
            global: global.clone(),
        };
        Ok((
            PreparedDomain {
                facility,
                spec,
                unit,
                records,
                global,
                emit,
            },
            snapshot,
        ))
    }

    /// Registers one committed domain (the second half of the open
    /// protocol: the consumer validated the records and built the
    /// domain with [`Domain::open`]).
    ///
    /// # Errors
    ///
    /// Rejects a name already registered.
    pub fn register(&self, domain: Domain) -> Result<(), FrStorageError> {
        let mut domains = self.domains.lock().map_err(locked_facility)?;
        if domains.contains_key(domain.name()) {
            return Err(FrStorageError::new(
                FrStorageErrorCode::AlreadyOpen,
                format!("domain '{}' is already open", domain.name()),
            ));
        }
        domains.insert(domain.name().to_owned(), domain);
        drop(domains);
        Ok(())
    }

    /// Releases one domain's name after a rejected consumer open.
    ///
    /// # Errors
    /// Propagates the facility lock failure.
    pub fn release(&self, name: &str) -> Result<(), FrStorageError> {
        let mut reserved = self.reserved.lock().map_err(locked_facility)?;
        reserved.remove(name);
        drop(reserved);
        Ok(())
    }

    /// Looks up one open domain by name.
    ///
    /// # Errors
    /// Propagates the facility lock failure.
    pub fn get(&self, name: &str) -> Result<Option<Domain>, FrStorageError> {
        let domains = self.domains.lock().map_err(locked_facility)?;
        Ok(domains.get(name).cloned())
    }

    /// Closes every open domain (the facility unmount path).
    ///
    /// # Errors
    /// Propagates the facility lock failure.
    pub fn close_all(&self) -> Result<(), FrStorageError> {
        let domains = {
            let mut slots = self.domains.lock().map_err(locked_facility)?;
            std::mem::take(&mut *slots)
        };
        for (_, domain) in domains {
            domain.close();
        }
        let mut reserved = self.reserved.lock().map_err(locked_facility)?;
        reserved.clear();
        drop(reserved);
        Ok(())
    }
}

impl std::fmt::Debug for DomainFacility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let open = self
            .domains
            .lock()
            .map(|slots| slots.keys().cloned().collect::<Vec<_>>());
        let pending = self
            .pending
            .lock()
            .map(|slots| slots.keys().cloned().collect::<Vec<_>>());
        f.debug_struct("DomainFacility")
            .field("open", &open.unwrap_or_default())
            .field("pending", &pending.unwrap_or_default())
            .finish_non_exhaustive()
    }
}

/// The raw stored snapshot the first open phase returns (the consumer
/// validates its records against its own schemas).
#[derive(Debug)]
pub struct SnapshotPayload {
    /// The validated spec name.
    pub name: String,
    /// The stored records per declared table (raw, unvalidated).
    pub records: HashMap<String, HashMap<String, Value>>,
    /// The stored global (raw, unvalidated); `None` until first written.
    pub global: Option<Value>,
}

/// A reserved open awaiting consumer record validation: everything
/// [`Domain::open`] needs, stashed until the commit or refusal.
pub struct PreparedDomain {
    facility: Arc<DomainFacility>,
    /// The validated spec.
    pub spec: DomainSpec,
    unit: SingleJsonUnit,
    /// The stored records per declared table (raw, unvalidated).
    pub records: HashMap<String, HashMap<String, Value>>,
    /// The stored global (raw, unvalidated); `None` until first written.
    pub global: Option<Value>,
    emit: EmitFace,
}

impl std::fmt::Debug for PreparedDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedDomain")
            .field("spec", &self.spec)
            .field("records", &self.records)
            .field("global", &self.global)
            .finish_non_exhaustive()
    }
}

impl PreparedDomain {
    /// Builds the domain over the prepared snapshot and registers it
    /// (the consumer's post-validation commit).
    ///
    /// # Errors
    ///
    /// Propagates [`Domain::open`] and facility registration failures;
    /// a rejected commit releases the name reservation.
    pub fn commit(self) -> Result<Domain, FrStorageError> {
        let name = self.spec.name.clone();
        let facility = Arc::clone(&self.facility);
        let on_close: OnClose = Arc::new(move || {
            // The onClosed hook runs strictly after teardown completes:
            // the name frees up for reopening only once the domain is
            // fully closed.
            if let Ok(mut domains) = facility.domains.lock() {
                domains.remove(&name);
            }
            if let Ok(mut reserved) = facility.reserved.lock() {
                reserved.remove(&name);
            }
        });
        let domain = Domain::open(
            &self.spec,
            self.unit,
            self.records,
            self.emit,
            Some(on_close),
        )?;
        self.facility.register(domain.clone())?;
        if let Ok(mut reserved) = self.facility.reserved.lock() {
            reserved.remove(domain.name());
        }
        Ok(domain)
    }

    /// Abandons the prepared open (the consumer's invalid-record
    /// refusal): releases the reservation and closes the unit.
    pub fn refuse(self) {
        self.unit.close();
        let _ = self.facility.release(&self.spec.name);
    }
}

/// The facility lock-poison diagnostic.
fn locked_facility(error: impl std::fmt::Display) -> FrStorageError {
    FrStorageError::malformed_medium(format!("domain facility lock poisoned: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// One mounted facility plus its captured event log.
    type Fixture = (Arc<DomainFacility>, PathBuf, Arc<Mutex<EventLog>>);
    /// The captured change log one facility fixture records.
    type EventLog = Vec<(String, &'static str)>;

    fn mounted() -> Fixture {
        let root =
            std::env::temp_dir().join(format!("fr-storage-facility-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let facility = Arc::new(DomainFacility::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let emit: EmitFace = Arc::new(move |changed| {
            sink.lock()
                .unwrap_or_else(|_| panic!("poisoned"))
                .push((changed.key.clone(), changed.operation.as_debug_name()));
        });
        facility
            .mount(root.clone(), emit)
            .unwrap_or_else(|e| panic!("{e}"));
        (facility, root, events)
    }

    fn spec() -> DomainSpec {
        DomainSpec {
            name: "schedule".to_owned(),
            version: 1,
            tables: vec!["tasks".to_owned()],
            has_global: false,
        }
    }

    #[test]
    fn mount_is_single_shot() {
        let (facility, root, _events) = mounted();
        let again = facility.mount(root.clone(), Arc::new(|_| {}));
        assert_eq!(again.unwrap_err().code, FrStorageErrorCode::AlreadyOpen);
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn the_two_phase_open_commits_and_reopens_durable() {
        let (facility, root, _events) = mounted();
        let snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        assert!(snapshot.records["tasks"].is_empty());
        let domain = facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .commit()
            .unwrap_or_else(|e| panic!("{e}"));
        domain
            .table("tasks")
            .unwrap_or_else(|e| panic!("{e}"))
            .put("k1", json!({"prompt": "p"}))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            facility
                .get("schedule")
                .unwrap_or_else(|e| panic!("{e}"))
                .is_some()
        );
        // A second open of the same name fails loud (the committed
        // domain holds the name).
        let error = facility.prepare(spec()).unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::AlreadyOpen);
        domain.close();
        // Reopen reads the durable state.
        let snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(snapshot.records["tasks"]["k1"], json!({"prompt": "p"}));
        facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .commit()
            .unwrap_or_else(|e| panic!("{e}"));
        facility.close_all().unwrap_or_else(|e| panic!("{e}"));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn a_refusal_releases_the_reservation() {
        let (facility, root, _events) = mounted();
        let _snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .refuse();
        let _snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .refuse();
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn close_all_joins_every_domain_worker() {
        let (facility, root, _events) = mounted();
        let _snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        let domain = facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .commit()
            .unwrap_or_else(|e| panic!("{e}"));
        domain
            .table("tasks")
            .unwrap_or_else(|e| panic!("{e}"))
            .put("x", json!(1))
            .unwrap_or_else(|e| panic!("{e}"));
        facility.close_all().unwrap_or_else(|e| panic!("{e}"));
        assert!(
            facility
                .get("schedule")
                .unwrap_or_else(|e| panic!("{e}"))
                .is_none()
        );
        // The facility stays mounted after close_all: a fresh open serves
        // the durable state the closed domain committed.
        let snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(snapshot.records["tasks"]["x"], json!(1));
        facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .refuse();
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn events_carry_durable_writes_in_order() {
        let (facility, root, events) = mounted();
        let _snapshot = facility.prepare(spec()).unwrap_or_else(|e| panic!("{e}"));
        let domain = facility
            .take_prepared("schedule")
            .unwrap_or_else(|e| panic!("{e}"))
            .commit()
            .unwrap_or_else(|e| panic!("{e}"));
        let table = domain.table("tasks").unwrap_or_else(|e| panic!("{e}"));
        table.put("a", json!(1)).unwrap_or_else(|e| panic!("{e}"));
        table.put("b", json!(2)).unwrap_or_else(|e| panic!("{e}"));
        table.delete("a").unwrap_or_else(|e| panic!("{e}"));
        let log = events.lock().unwrap_or_else(|_| panic!("poisoned"));
        assert_eq!(
            log.as_slice(),
            &[
                ("a".to_owned(), "put"),
                ("b".to_owned(), "put"),
                ("a".to_owned(), "deleted"),
            ]
        );
        facility.close_all().unwrap_or_else(|e| panic!("{e}"));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }
}
