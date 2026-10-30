//! One opened JSON unit in `single` layout: the whole unit is one
//! document at `<root>/<name>.json` (the dsh `storage-json`
//! `single-unit.ts`).
//!
//! The in-memory state is authoritative; every write primitive mutates
//! it and republishes the whole file atomically. Write ordering belongs
//! to the caller (the domain layer's write chain); this unit only
//! guarantees that each single call publishes a complete, durable
//! file.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use crate::atomic::write_atomic;
use crate::error::{FrStorageError, FrStorageErrorCode};
use crate::format::{UnitState, parse, serialize};

/// Static identity and shape of one unit (the dsh `KvUnitDescriptor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitDescriptor {
    /// Unit name; must match the unit name pattern (also the file segment).
    pub name: String,
    /// Unit format version; stamped on the medium at first materialization.
    pub version: u32,
    /// Declared table names.
    pub tables: Vec<String>,
    /// Whether this unit carries the global singleton slot.
    pub has_global: bool,
}

/// The backend face of one opened unit: snapshot reads plus per-record
/// durable writes over one JSON document.
#[derive(Debug)]
pub struct SingleJsonUnit {
    descriptor: UnitDescriptor,
    path: PathBuf,
    /// Authoritative in-memory state behind a mutex.
    state: Mutex<UnitState>,
    /// Closure stamp; every call after it rejects with `closed`.
    closed: std::sync::atomic::AtomicBool,
}

impl SingleJsonUnit {
    /// Opens (loads or lazily creates) one unit under `root`:
    /// `<root>/<name>.json`. A missing file reads as an empty unit whose
    /// materialization defers to the first write.
    ///
    /// # Errors
    ///
    /// Propagates read errors other than the missing file and any parse
    /// rejection (`malformed-medium`/`version-mismatch`).
    pub fn open(descriptor: UnitDescriptor, root: &Path) -> Result<Self, FrStorageError> {
        let path = root.join(format!("{}.json", descriptor.name));
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(FrStorageError::malformed_medium(format!(
                    "unit '{}': read failed: {error}",
                    descriptor.name
                )));
            }
        };
        let state = match text {
            None => UnitState {
                version: descriptor.version,
                global: None,
                tables: descriptor
                    .tables
                    .iter()
                    .cloned()
                    .map(|table| (table, Map::new()))
                    .collect(),
            },
            Some(ref text) => parse(
                text,
                &descriptor.name,
                descriptor.version,
                &descriptor.tables,
            )?,
        };
        Ok(Self {
            descriptor,
            path,
            state: Mutex::new(state),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Reads the full current snapshot.
    ///
    /// # Errors
    ///
    /// Rejects a closed unit with `closed`.
    pub fn load_all(&self) -> Result<(Map<String, Value>, Option<Value>), FrStorageError> {
        let state = self.guard()?;
        let tables = state
            .tables
            .iter()
            .map(|(table, records)| (table.clone(), Value::Object(records.clone())))
            .collect();
        Ok((tables, state.global.clone()))
    }

    /// Upserts one record durably (overwrite semantics).
    ///
    /// # Errors
    ///
    /// Rejects a closed unit and an undeclared table; a failed publish
    /// rolls the memory change back before propagating.
    pub fn put_record(&self, table: &str, key: &str, value: Value) -> Result<(), FrStorageError> {
        let (previous, body) = {
            let mut state = self.guard()?;
            let previous = {
                let records = self.records(&mut state, table)?;
                records.insert(key.to_owned(), value)
            };
            let body = serialize(&self.descriptor.name, &state);
            drop(state);
            (previous, body)
        };
        if let Err(error) = self.publish(&body) {
            self.rollback_put(table, key, previous)?;
            return Err(error);
        }
        Ok(())
    }

    /// Rolls one failed put back under a fresh state lock.
    fn rollback_put(
        &self,
        table: &str,
        key: &str,
        previous: Option<Value>,
    ) -> Result<(), FrStorageError> {
        let mut state = self.guard()?;
        {
            let records = self.records(&mut state, table)?;
            match previous {
                Some(previous) => {
                    records.insert(key.to_owned(), previous);
                }
                None => {
                    records.remove(key);
                }
            }
        }
        drop(state);
        Ok(())
    }

    /// Deletes one record durably (idempotent on a missing key).
    ///
    /// # Errors
    ///
    /// Rejects a closed unit and an undeclared table; a failed publish
    /// rolls the memory change back before propagating.
    pub fn delete_record(&self, table: &str, key: &str) -> Result<(), FrStorageError> {
        let (previous, body) = {
            let mut state = self.guard()?;
            let previous = {
                let records = self.records(&mut state, table)?;
                records.remove(key)
            };
            let body = serialize(&self.descriptor.name, &state);
            drop(state);
            (previous, body)
        };
        let Some(previous) = previous else {
            return Ok(());
        };
        if let Err(error) = self.publish(&body) {
            self.rollback_delete(table, key, previous)?;
            return Err(error);
        }
        Ok(())
    }

    /// Rolls one failed delete back under a fresh state lock.
    fn rollback_delete(
        &self,
        table: &str,
        key: &str,
        previous: Value,
    ) -> Result<(), FrStorageError> {
        let mut state = self.guard()?;
        {
            let records = self.records(&mut state, table)?;
            records.insert(key.to_owned(), previous);
        }
        drop(state);
        Ok(())
    }

    /// Writes the global singleton durably (declared slots only).
    ///
    /// # Errors
    ///
    /// Rejects a closed unit, a descriptor without the global slot, and a
    /// failed publish (which rolls the memory change back first).
    pub fn set_global(&self, value: Value) -> Result<(), FrStorageError> {
        let (previous, body) = {
            let mut state = self.guard()?;
            if !self.descriptor.has_global {
                return Err(FrStorageError::new(
                    FrStorageErrorCode::MissingKey,
                    format!(
                        "unit '{}' does not declare a global slot",
                        self.descriptor.name
                    ),
                ));
            }
            let previous = state.global.replace(value);
            let body = serialize(&self.descriptor.name, &state);
            drop(state);
            (previous, body)
        };
        if let Err(error) = self.publish(&body) {
            let mut state = self.guard()?;
            state.global = previous;
            drop(state);
            return Err(error);
        }
        Ok(())
    }

    /// Drains the unit and releases it (idempotent).
    ///
    /// The dsh unit drains in-flight publishes before releasing; the
    /// synchronous fr unit publishes inline under the state lock, so the
    /// close takes the lock once and stamps closure (reads and writes
    /// reject from here; the dsh drain semantics collapse because fr
    /// publishes are synchronous).
    pub fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl SingleJsonUnit {
    /// Locks the state, rejecting reads and writes after closure.
    fn guard(&self) -> Result<MutexGuard<'_, UnitState>, FrStorageError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(FrStorageError::closed(format!(
                "unit '{}' is closed",
                self.descriptor.name
            )));
        }
        self.state.lock().map_err(|error| {
            FrStorageError::new(
                FrStorageErrorCode::MalformedMedium,
                format!(
                    "unit '{}' state lock poisoned: {error}",
                    self.descriptor.name
                ),
            )
        })
    }

    /// Resolves one declared table's record map under the state lock.
    fn records<'a>(
        &self,
        state: &'a mut UnitState,
        table: &str,
    ) -> Result<&'a mut Map<String, Value>, FrStorageError> {
        state.tables.get_mut(table).ok_or_else(|| {
            FrStorageError::new(
                FrStorageErrorCode::MissingKey,
                format!(
                    "unit '{}' does not declare table '{table}'",
                    self.descriptor.name
                ),
            )
        })
    }

    /// The unit descriptor (identity and declared shape).
    #[must_use]
    pub const fn descriptor(&self) -> &UnitDescriptor {
        &self.descriptor
    }

    /// Whether the descriptor declares the global slot.
    #[must_use]
    pub const fn declared_global(&self) -> bool {
        self.descriptor.has_global
    }

    /// Publishes one serialized unit body atomically to the unit file.
    fn publish(&self, body: &str) -> Result<(), FrStorageError> {
        write_atomic(&self.path, body).map_err(|error| {
            FrStorageError::malformed_medium(format!(
                "unit '{}' publish failed: {error}",
                self.descriptor.name
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("fr-storage-unit-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
        root
    }

    fn descriptor(name: &str) -> UnitDescriptor {
        UnitDescriptor {
            name: name.to_owned(),
            version: 1,
            tables: vec!["tasks".to_owned()],
            has_global: false,
        }
    }

    #[test]
    fn missing_file_opens_empty_and_first_write_materializes() {
        let root = scratch_root("empty");
        let unit =
            SingleJsonUnit::open(descriptor("schedule"), &root).unwrap_or_else(|e| panic!("{e}"));
        let path = root.join("schedule.json");
        assert!(!path.exists());
        unit.put_record("tasks", "k1", json!({"prompt": "hello"}))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(text.starts_with('{'));
        assert!(text.ends_with("}\n"));
        let state = crate::format::parse(&text, "schedule", 1, &["tasks".to_owned()])
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(state.tables["tasks"]["k1"], json!({"prompt": "hello"}));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn reopen_reads_the_stored_snapshot() {
        let root = scratch_root("reopen");
        {
            let unit = SingleJsonUnit::open(descriptor("schedule"), &root)
                .unwrap_or_else(|e| panic!("{e}"));
            unit.put_record("tasks", "a", json!(1))
                .unwrap_or_else(|e| panic!("{e}"));
            unit.put_record("tasks", "b", json!(2))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        let unit =
            SingleJsonUnit::open(descriptor("schedule"), &root).unwrap_or_else(|e| panic!("{e}"));
        let (tables, global) = unit.load_all().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            tables["tasks"].as_object().map(serde_json::Map::len),
            Some(2)
        );
        assert_eq!(global, None);
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn delete_is_idempotent_and_global_requires_the_slot() {
        let root = scratch_root("delete");
        let unit =
            SingleJsonUnit::open(descriptor("schedule"), &root).unwrap_or_else(|e| panic!("{e}"));
        unit.put_record("tasks", "gone", json!(true))
            .unwrap_or_else(|e| panic!("{e}"));
        unit.delete_record("tasks", "gone")
            .unwrap_or_else(|e| panic!("{e}"));
        unit.delete_record("tasks", "gone")
            .unwrap_or_else(|e| panic!("{e}"));
        let (tables, _) = unit.load_all().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            tables["tasks"].as_object().map(serde_json::Map::len),
            Some(0)
        );
        let error = unit.set_global(json!(1)).unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::MissingKey);
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn a_failed_publish_rolls_the_memory_change_back() {
        let root = scratch_root("rollback");
        let unit =
            SingleJsonUnit::open(descriptor("schedule"), &root).unwrap_or_else(|e| panic!("{e}"));
        unit.put_record("tasks", "keep", json!(true))
            .unwrap_or_else(|e| panic!("{e}"));
        // The target file becomes a directory: the rename inside the
        // atomic publish fails and the put must not survive in memory.
        std::fs::remove_file(root.join("schedule.json")).unwrap_or_else(|e| panic!("{e}"));
        std::fs::create_dir_all(root.join("schedule.json")).unwrap_or_else(|e| panic!("{e}"));
        let error = unit
            .put_record("tasks", "doomed", json!(false))
            .unwrap_err();
        assert_ne!(error.message, String::new());
        let (tables, _) = unit.load_all().unwrap_or_else(|e| panic!("{e}"));
        assert!(tables["tasks"].get("doomed").is_none());
        assert_eq!(tables["tasks"].get("keep"), Some(&json!(true)));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn close_rejects_every_further_call() {
        let root = scratch_root("closed");
        let unit =
            SingleJsonUnit::open(descriptor("schedule"), &root).unwrap_or_else(|e| panic!("{e}"));
        unit.close();
        let error = unit.put_record("tasks", "late", json!(1)).unwrap_err();
        assert_eq!(error.code, FrStorageErrorCode::Closed);
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn serialize_keeps_insertion_order() {
        let mut records = serde_json::Map::new();
        records.insert("zeta".to_owned(), json!(1));
        records.insert("alpha".to_owned(), json!(2));
        let state = UnitState {
            version: 1,
            global: None,
            tables: std::iter::once(("tasks".to_owned(), records)).collect(),
        };
        let text = serialize("schedule", &state);
        let zeta_at = text.find("zeta").unwrap_or_default();
        let alpha_at = text.find("alpha").unwrap_or_default();
        assert!(zeta_at < alpha_at);
    }
}
