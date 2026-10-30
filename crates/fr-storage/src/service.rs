//! The `storage-domain` service face: JSON-in/JSON-out method dispatch
//! over the facility, so the schedule consumer reaches domains across
//! the dynamic channel.

#![forbid(unsafe_code)]

use std::sync::Arc;

use serde_json::{Value, json};

use crate::domain::{Domain, DomainSpec};
use crate::facility::DomainFacility;

/// The plugin name the host registers the artifact under.
pub const PLUGIN_NAME: &str = "fr-storage";

/// Method 0: the service face description.
pub const METHOD_DESCRIBE: u32 = 0;
/// Method 1: opens one declared domain.
pub const METHOD_OPEN: u32 = 1;
/// Method 2: reads the validated full snapshot.
pub const METHOD_LOAD_ALL: u32 = 2;
/// Method 3: reads one record.
pub const METHOD_GET_RECORD: u32 = 3;
/// Method 4: snapshots one table's entries in insertion order.
pub const METHOD_TABLE_ENTRIES: u32 = 4;
/// Method 5: upserts one record durably.
pub const METHOD_PUT_RECORD: u32 = 5;
/// Method 6: deletes one record durably.
pub const METHOD_DELETE_RECORD: u32 = 6;
/// Method 7: writes the global singleton durably.
pub const METHOD_SET_GLOBAL: u32 = 7;
/// Method 8: reads the global singleton.
pub const METHOD_GLOBAL_GET: u32 = 8;
/// Method 9: closes one domain.
pub const METHOD_CLOSE: u32 = 9;
/// Method 10: commits one prepared open after consumer validation.
pub const METHOD_COMMIT: u32 = 10;
/// Method 11: refuses one prepared open after an invalid record.
pub const METHOD_REFUSE: u32 = 11;

/// The dispatch face (pure JSON-in/JSON-out so the surface stays
/// testable).
#[derive(Debug)]
pub struct StorageDomainService {
    facility: Arc<DomainFacility>,
}

impl StorageDomainService {
    /// Wraps one facility.
    #[must_use]
    pub const fn new(facility: Arc<DomainFacility>) -> Self {
        Self { facility }
    }

    /// Serves one method invocation.
    ///
    /// # Errors
    ///
    /// Returns the failure text the host surfaces as `STATUS_FAILED`.
    pub fn serve(&self, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
        let request: Value = if payload.is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_slice(payload)
                .map_err(|error| format!("payload must be JSON: {error}"))?
        };
        let reply = match method {
            METHOD_DESCRIBE => self.describe(),
            METHOD_OPEN => self.open(&request)?,
            METHOD_LOAD_ALL => self.load_all(&request)?,
            METHOD_GET_RECORD => self.get_record(&request)?,
            METHOD_TABLE_ENTRIES => self.table_entries(&request)?,
            METHOD_PUT_RECORD => self.put_record(&request)?,
            METHOD_DELETE_RECORD => self.delete_record(&request)?,
            METHOD_SET_GLOBAL => self.set_global(&request)?,
            METHOD_GLOBAL_GET => self.global_get(&request)?,
            METHOD_CLOSE => self.close(&request)?,
            METHOD_COMMIT => self.commit(&request)?,
            METHOD_REFUSE => self.refuse(&request)?,
            other => return Err(format!("unknown {PLUGIN_NAME} method {other}")),
        };
        serde_json::to_vec(&reply).map_err(|error| format!("reply encode failed: {error}"))
    }

    /// The service face description (the fr-shell describe shape).
    #[must_use]
    pub fn describe(&self) -> Value {
        json!({
            "name": PLUGIN_NAME,
            "service": "storage-domain",
            "methods": {
                "describe": METHOD_DESCRIBE,
                "open": METHOD_OPEN,
                "load_all": METHOD_LOAD_ALL,
                "get_record": METHOD_GET_RECORD,
                "table_entries": METHOD_TABLE_ENTRIES,
                "put_record": METHOD_PUT_RECORD,
                "delete_record": METHOD_DELETE_RECORD,
                "set_global": METHOD_SET_GLOBAL,
                "global_get": METHOD_GLOBAL_GET,
                "close": METHOD_CLOSE,
                "commit": METHOD_COMMIT,
                "refuse": METHOD_REFUSE,
            },
        })
    }

    /// Prepares one declared domain and returns the raw stored snapshot:
    /// the consumer validates the records against its own schemas, then
    /// completes the open with [`Self::commit`] or [`Self::refuse`].
    fn open(&self, request: &Value) -> Result<Value, String> {
        let spec = DomainSpec {
            name: string_field(request, "name")?,
            version: u32_field(request, "version")?,
            tables: strings_field(request, "tables")?,
            has_global: request
                .get("hasGlobal")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        let snapshot = self
            .facility
            .prepare(spec)
            .map_err(|error| error.to_string())?;
        let mut records = serde_json::Map::new();
        for (table, table_records) in &snapshot.records {
            let mut slot = serde_json::Map::new();
            for (key, value) in table_records {
                slot.insert(key.clone(), value.clone());
            }
            records.insert(table.clone(), Value::Object(slot));
        }
        Ok(json!({
            "name": snapshot.name,
            "records": Value::Object(records),
            "global": snapshot.global,
        }))
    }

    /// Commits one prepared open (the consumer validated the records).
    fn commit(&self, request: &Value) -> Result<Value, String> {
        let name = string_field(request, "name")?;
        let prepared = self
            .facility
            .take_prepared(&name)
            .map_err(|error| error.to_string())?;
        prepared.commit().map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true }))
    }

    /// Refuses one prepared open (the consumer hit an invalid record).
    fn refuse(&self, request: &Value) -> Result<Value, String> {
        let name = string_field(request, "name")?;
        let prepared = self
            .facility
            .take_prepared(&name)
            .map_err(|error| error.to_string())?;
        prepared.refuse();
        Ok(json!({ "ok": true }))
    }

    /// Reads the validated full snapshot of one open domain.
    fn load_all(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let tables = domain_snapshot(&domain)?;
        let global = domain.global_get().map_err(|error| error.to_string())?;
        Ok(json!({ "tables": tables, "global": global }))
    }

    /// Reads one record from one open domain.
    fn get_record(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let table = string_field(request, "table")?;
        let key = string_field(request, "key")?;
        let value = domain
            .get(&table, &key)
            .map_err(|error| error.to_string())?
            .unwrap_or(Value::Null);
        Ok(json!({ "value": value }))
    }

    /// Snapshots one table's entries in insertion order.
    fn table_entries(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let table = string_field(request, "table")?;
        let entries = domain
            .entries(&table)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect::<Vec<_>>();
        Ok(json!({ "entries": entries }))
    }

    /// Upserts one record on one open domain.
    fn put_record(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let table = string_field(request, "table")?;
        let key = string_field(request, "key")?;
        let value = request
            .get("value")
            .cloned()
            .ok_or_else(|| "payload requires a value".to_owned())?;
        domain
            .table(&table)
            .map_err(|error| error.to_string())?
            .put(&key, value)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true }))
    }

    /// Deletes one record on one open domain.
    fn delete_record(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let table = string_field(request, "table")?;
        let key = string_field(request, "key")?;
        let deleted = domain
            .table(&table)
            .map_err(|error| error.to_string())?
            .delete(&key)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "deleted": deleted }))
    }

    /// Writes the global singleton on one open domain.
    fn set_global(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let value = request
            .get("value")
            .cloned()
            .ok_or_else(|| "payload requires a value".to_owned())?;
        domain
            .global_set(value)
            .map_err(|error| error.to_string())?;
        Ok(json!({ "ok": true }))
    }

    /// Reads the global singleton from one open domain.
    fn global_get(&self, request: &Value) -> Result<Value, String> {
        let domain = self.open_domain(request)?;
        let value = domain
            .global_get()
            .map_err(|error| error.to_string())?
            .unwrap_or(Value::Null);
        Ok(json!({ "value": value }))
    }

    /// Closes one open domain.
    fn close(&self, request: &Value) -> Result<Value, String> {
        let name = string_field(request, "domain")?;
        let domain = self
            .facility
            .get(&name)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("domain '{name}' is not open"))?;
        domain.close();
        Ok(json!({ "ok": true }))
    }

    /// Resolves one open domain from the request's `domain` field.
    fn open_domain(&self, request: &Value) -> Result<Domain, String> {
        let name = string_field(request, "domain")?;
        self.facility
            .get(&name)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("domain '{name}' is not open"))
    }
}

/// Builds the snapshot JSON object of one open domain's tables.
/// Falls back to the declared-table walk when the domain refuses reads.
fn domain_snapshot(domain: &Domain) -> Result<Value, String> {
    let declared = domain
        .snapshot_tables()
        .map_err(|error| error.to_string())?;
    let mut tables = serde_json::Map::new();
    for (table, records) in declared {
        let mut slot = serde_json::Map::new();
        for (key, value) in records {
            slot.insert(key, value);
        }
        tables.insert(table, Value::Object(slot));
    }
    Ok(Value::Object(tables))
}

/// Reads one required string field.
fn string_field(request: &Value, field: &str) -> Result<String, String> {
    request
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("payload requires a string '{field}'"))
}

/// Reads one required u32 field.
fn u32_field(request: &Value, field: &str) -> Result<u32, String> {
    request
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("payload requires a u32 '{field}'"))
}

/// Reads one required string-array field (each entry must be a string).
fn strings_field(request: &Value, field: &str) -> Result<Vec<String>, String> {
    let values = request
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("payload requires a string array '{field}'"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("payload field '{field}' carries a non-string entry"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn service(tag: &str) -> (StorageDomainService, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("fr-storage-service-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
        let facility = Arc::new(DomainFacility::new());
        facility
            .mount(root.clone(), Arc::new(|_| {}))
            .unwrap_or_else(|e| panic!("{e}"));
        (StorageDomainService::new(facility), root)
    }

    fn serve_json(
        service: &StorageDomainService,
        method: u32,
        payload: &Value,
    ) -> Result<Value, String> {
        let bytes = serde_json::to_vec(&payload).unwrap_or_else(|e| panic!("{e}"));
        let reply = service.serve(method, &bytes)?;
        serde_json::from_slice(&reply).map_err(|error| format!("reply decode failed: {error}"))
    }

    #[test]
    fn describe_lists_the_method_table() {
        let (service, root) = service("describe");
        let reply = service
            .serve(METHOD_DESCRIBE, &[])
            .unwrap_or_else(|e| panic!("{e}"));
        let value: Value = serde_json::from_slice(&reply).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(value["name"], PLUGIN_NAME);
        assert_eq!(value["service"], "storage-domain");
        assert_eq!(value["methods"]["open"], METHOD_OPEN);
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn an_unknown_method_names_itself() {
        let (service, root) = service("unknown");
        let error = service.serve(999, &[]).unwrap_err();
        assert!(error.contains("unknown fr-storage method 999"));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn the_wire_open_round_trip_carries_records() {
        let (service, root) = service("roundtrip");
        let snapshot = serve_json(
            &service,
            METHOD_OPEN,
            &json!({
                "name": "schedule", "version": 1, "tables": ["tasks"], "hasGlobal": false,
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(snapshot["records"]["tasks"], json!({}));
        serve_json(&service, METHOD_COMMIT, &json!({"name": "schedule"}))
            .unwrap_or_else(|e| panic!("{e}"));
        serve_json(
            &service,
            METHOD_PUT_RECORD,
            &json!({
                "domain": "schedule", "table": "tasks", "key": "k1", "value": {"prompt": "p"},
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let got = serve_json(
            &service,
            METHOD_GET_RECORD,
            &json!({
                "domain": "schedule", "table": "tasks", "key": "k1",
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got["value"], json!({"prompt": "p"}));
        let entries = serve_json(
            &service,
            METHOD_TABLE_ENTRIES,
            &json!({
                "domain": "schedule", "table": "tasks",
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(entries["entries"].as_array().map(Vec::len), Some(1));
        let deleted = serve_json(
            &service,
            METHOD_DELETE_RECORD,
            &json!({
                "domain": "schedule", "table": "tasks", "key": "k1",
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(deleted["deleted"], json!(true));
        serve_json(&service, METHOD_CLOSE, &json!({"domain": "schedule"}))
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn an_invalid_record_refusal_releases_the_name() {
        let (service, root) = service("refuse");
        let _snapshot = serve_json(
            &service,
            METHOD_OPEN,
            &json!({
                "name": "schedule", "version": 1, "tables": ["tasks"], "hasGlobal": false,
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        serve_json(&service, METHOD_REFUSE, &json!({"name": "schedule"}))
            .unwrap_or_else(|e| panic!("{e}"));
        // The name is free again: the second open prepares.
        let again = serve_json(
            &service,
            METHOD_OPEN,
            &json!({
                "name": "schedule", "version": 1, "tables": ["tasks"], "hasGlobal": false,
            }),
        );
        let reopened = again.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reopened["records"]["tasks"], json!({}));
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }
}
