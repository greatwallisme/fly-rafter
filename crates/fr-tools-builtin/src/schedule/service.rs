// Rust guideline compliant 2026-10-31
//! The Host storage service (dsh `index.ts` `ScheduleService` + `update.ts`
//! `resolveScheduleUpdate` + `storage.ts` task validation): the durable
//! task rows over the fr-storage domain, the four management operations,
//! and the optimistic timing/content update.

use std::sync::Mutex;

use serde_json::{Value, json};
use uuid::Uuid;

use super::domain::{
    ScheduleError, ScheduleInputError, ScheduleLogError, ScheduleRecord,
    canonicalize_time_zone_name, create_after_schedule_record, create_at_schedule_record,
    create_cron_schedule_record, create_daily_schedule_record, create_every_schedule_record,
    create_weekly_schedule_record, fixed, parse_cron_input, parse_daily_input, parse_weekly_input,
    retained_title, schedule_prompt, schedule_title,
};
#[cfg(test)]
use super::occurrences::append_delivery;

use super::occurrences::{
    DeliveryHistory, DeliveryReceipt, DeliveryRecord, decode_schedule_record,
};

/// The storage domain name (dsh `scheduleDomain`).
pub const DOMAIN_NAME: &str = "schedule";

/// The domain version (dsh v1, single `tasks` table).
pub const DOMAIN_VERSION: u32 = 1;

/// The one declared table.
pub const DOMAIN_TABLE: &str = "tasks";

/// Retained delivery-history window applied when the deployment states
/// none (dsh `DEFAULT_DELIVERY_HISTORY_DAYS`).
pub const DEFAULT_DELIVERY_HISTORY_DAYS: i64 = 30;

/// Retained delivery-record cap applied when the deployment states none
/// (dsh `DEFAULT_DELIVERY_HISTORY_RECORDS`).
pub const DEFAULT_DELIVERY_HISTORY_RECORDS: i64 = 200;

/// The cross-plugin faces the service consumes (dsh `ctx.agents`,
/// `ctx.storageDomain`, `ctx.sessions` + emit + the delegation depth).
pub trait ScheduleSeams: std::fmt::Debug + Send + Sync {
    /// Snapshots the task table's rows in insertion order.
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    fn table_entries(&self) -> Result<Vec<(String, Value)>, String>;

    /// Upserts one task row durably.
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    fn put_task(&self, id: &str, task: &Value) -> Result<(), String>;

    /// Deletes one task row durably.
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    fn delete_task(&self, id: &str) -> Result<(), String>;

    /// Publishes the durable change notification (dsh
    /// `schedule/changed`).
    fn emit_changed(&self);

    /// The delegation depth of one session (dsh `delegationDepthOf`;
    /// M1 wiring reports zero until the delegation registry lands).
    fn delegation_depth(&self, session: &str) -> u32;

    /// Queues one follow-up input (dsh `agent.followup`; `NextTurn` plus
    /// wake).
    ///
    /// # Errors
    ///
    /// The delivery refusal text.
    fn followup(&self, session: &str, message: &Value) -> Result<(), String>;

    /// Runs the flush checkpoint for one session (dsh
    /// `sessions.flush`).
    ///
    /// # Errors
    ///
    /// The persistence refusal text.
    fn flush(&self, session: &str) -> Result<(), String>;
}

/// One validated stored task (dsh `ScheduleTask`).
#[derive(Debug, Clone)]
pub struct ScheduleTask {
    /// Original session binding.
    pub(crate) session_id: String,
    /// The rule and its committed target.
    pub(crate) record: ScheduleRecord,
    /// Inactive tasks remain visible but never deliver.
    pub(crate) status: String,
    /// Most recent durably acknowledged delivery, when available.
    pub(crate) last_delivery: Option<DeliveryReceipt>,
    /// Retained saved deliveries, when present.
    pub(crate) history: Option<DeliveryHistory>,
}

impl ScheduleTask {
    /// The durable row form (dsh construction order).
    pub(crate) fn to_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("sessionId".to_owned(), json!(self.session_id));
        object.insert("record".to_owned(), self.record.to_value());
        object.insert("status".to_owned(), json!(self.status));
        if let Some(receipt) = &self.last_delivery {
            object.insert(
                "lastDelivery".to_owned(),
                json!({
                    "scheduledAt": receipt.scheduled_at,
                    "deliveredAt": receipt.delivered_at,
                    "messageId": receipt.message_id,
                }),
            );
        }
        if let Some(history) = &self.history {
            object.insert("deliveryHistory".to_owned(), history.to_value());
        }
        Value::Object(object)
    }

    /// Decodes one stored row (dsh `scheduleTaskSchema`): the strict
    /// key set, the record decode, the receipt/history shapes, and the
    /// last-delivery-matches-latest refinement.
    ///
    /// # Errors
    ///
    /// Malformed stored data (the open refuses).
    #[expect(
        clippy::too_many_lines,
        reason = "the decode mirrors the dsh schema one refinement per block; splitting it would hide the stored-shape correspondence"
    )]
    pub(crate) fn from_value(value: &Value) -> Result<Self, ScheduleLogError> {
        let object = value
            .as_object()
            .ok_or_else(|| ScheduleLogError("task must be an object".to_owned()))?;
        for key in ["sessionId", "record", "status"] {
            if !object.contains_key(key) {
                return Err(ScheduleLogError(
                    "task must contain exactly sessionId, record, status, and optional lastDelivery plus deliveryHistory".to_owned(),
                ));
            }
        }
        let session_id = value
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| ScheduleLogError("sessionId must be a non-empty string".to_owned()))?
            .to_owned();
        if session_id.is_empty() {
            return Err(ScheduleLogError(
                "sessionId must be a non-empty string".to_owned(),
            ));
        }
        let record = decode_schedule_record(value.get("record").unwrap_or(&Value::Null))?;
        for key in object.keys() {
            if !matches!(
                key.as_str(),
                "sessionId" | "record" | "status" | "lastDelivery" | "deliveryHistory"
            ) {
                return Err(ScheduleLogError(format!(
                    "task carries an unexpected key {key}"
                )));
            }
        }
        let status = match value.get("status") {
            None | Some(Value::Null) => "active".to_owned(),
            Some(status) => match status.as_str() {
                Some("active") => "active".to_owned(),
                Some("inactive") => "inactive".to_owned(),
                _ => {
                    return Err(ScheduleLogError(
                        "status must be active or inactive".to_owned(),
                    ));
                }
            },
        };
        let last_delivery = match value.get("lastDelivery") {
            None | Some(Value::Null) => None,
            Some(receipt) => Some(
                DeliveryRecord::from_value(receipt)
                    .map_err(|error| ScheduleLogError(format!("lastDelivery: {}", error.0)))?
                    .receipt,
            ),
        };
        let history = match value.get("deliveryHistory") {
            None | Some(Value::Null) => None,
            Some(history) => {
                let history_object = history.as_object().ok_or_else(|| {
                    ScheduleLogError("deliveryHistory must be an object".to_owned())
                })?;
                if !history_object.contains_key("records")
                    || !history_object.contains_key("earlierRecordsUnavailable")
                {
                    return Err(ScheduleLogError(
                        "deliveryHistory must contain exactly records, earlierRecordsUnavailable, and optional earlierRecordsPruned".to_owned(),
                    ));
                }
                let mut records = Vec::new();
                for entry in history
                    .get("records")
                    .and_then(Value::as_array)
                    .unwrap_or(&Vec::new())
                {
                    records.push(DeliveryRecord::from_value(entry).map_err(|error| {
                        ScheduleLogError(format!("deliveryHistory: {}", error.0))
                    })?);
                }
                let identities: std::collections::BTreeSet<&str> = records
                    .iter()
                    .map(|record| record.receipt.message_id.as_str())
                    .collect();
                if identities.len() != records.len() {
                    return Err(ScheduleLogError(
                        "Delivery history message identities must be unique within a task"
                            .to_owned(),
                    ));
                }
                Some(DeliveryHistory {
                    records,
                    earlier_records_unavailable: history
                        .get("earlierRecordsUnavailable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    earlier_records_pruned: history
                        .get("earlierRecordsPruned")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            }
        };
        let task = Self {
            session_id,
            record,
            status,
            last_delivery,
            history,
        };
        if let Some(history) = &task.history {
            let matches_latest = match history.records.last() {
                None => task.last_delivery.is_none(),
                Some(latest) => task.last_delivery.as_ref().is_some_and(|last| {
                    last.scheduled_at == latest.receipt.scheduled_at
                        && last.delivered_at == latest.receipt.delivered_at
                        && last.message_id == latest.receipt.message_id
                }),
            };
            if !matches_latest {
                return Err(ScheduleLogError(
                    "Last delivery must match the latest saved delivery receipt".to_owned(),
                ));
            }
        }
        Ok(task)
    }
}

/// The Host schedule service (dsh `ScheduleService`): the durable task
/// rows over the fr-storage domain, serialized management operations,
/// and the shared delivery-retention bounds.
pub struct Schedules {
    retention: (i64, i64),
    chain: Mutex<()>,
    seams: Box<dyn ScheduleSeams>,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    signal: std::sync::OnceLock<std::sync::Arc<super::runtime::RuntimeSignal>>,
}

impl std::fmt::Debug for Schedules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schedules").finish_non_exhaustive()
    }
}

impl Schedules {
    /// Assembles the service over the wire seams under the real wall
    /// clock (the baked retention defaults; the dsh base bundle adds no
    /// schedule config rows).
    ///
    /// # Errors
    ///
    /// The retention bounds are constants in range, so the static
    /// resolution never refuses in practice.
    #[must_use]
    pub fn new(seams: Box<dyn ScheduleSeams>) -> Self {
        Self::with_clock(
            seams,
            (
                DEFAULT_DELIVERY_HISTORY_DAYS,
                DEFAULT_DELIVERY_HISTORY_RECORDS,
            ),
            Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| {
                        i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
                    })
            }),
        )
    }

    /// Assembles the service under an injected clock and retention pair
    /// (the tests).
    pub(crate) fn with_clock(
        seams: Box<dyn ScheduleSeams>,
        retention: (i64, i64),
        now: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        Self {
            retention,
            chain: Mutex::new(()),
            seams,
            now,
            signal: std::sync::OnceLock::new(),
        }
    }

    /// Binds the delivery runtime's recompute signal (the shell sets it
    /// right after construction, before any caller can observe the
    /// service).
    pub(crate) fn bind_signal(&self, signal: std::sync::Arc<super::runtime::RuntimeSignal>) {
        let _ignored = self.signal.set(signal);
    }

    /// The hosted seams (the runtime consumes them directly).
    pub(crate) fn seams(&self) -> &dyn ScheduleSeams {
        self.seams.as_ref()
    }

    /// The retention bounds (the runtime's append shares them).
    pub(crate) const fn retention(&self) -> (i64, i64) {
        self.retention
    }

    /// Snapshots the stored rows (the runtime's task read).
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    pub(crate) fn tasks(&self) -> Result<Vec<(String, ScheduleTask)>, ScheduleLogError> {
        self.seams
            .table_entries()
            .map_err(ScheduleLogError)?
            .into_iter()
            .map(|(key, value)| Ok((key, ScheduleTask::from_value(&value)?)))
            .collect()
    }

    /// The stable refusal for a session no delivery can reach (dsh
    /// `reminderTargetRefusal`; the depth wiring rides the seams).
    fn reminder_target_refusal(&self, session: &str) -> Option<ScheduleInputError> {
        (self.seams.delegation_depth(session) > 0).then(|| {
            ScheduleInputError::new(
                "subagent_session",
                "This Session belongs to subagent routing, which never receives reminder delivery.",
            )
        })
    }

    /// Creates one reminder bound to the caller-selected session (dsh
    /// `create`): the clock samples before the serialized slot, so a
    /// create that waits keeps its request-time anchor.
    ///
    /// # Errors
    ///
    /// The selector, content, rule, range, or subagent refusals.
    #[expect(
        clippy::too_many_lines,
        reason = "the create mirrors the dsh selector chain one rule per arm; splitting it would hide the precedence"
    )]
    pub(crate) fn create(
        &self,
        session_id: &str,
        request: &Value,
    ) -> Result<ScheduleRecord, ScheduleError> {
        let selector_count = [
            "after_seconds",
            "at",
            "every_seconds",
            "daily",
            "weekly",
            "cron",
        ]
        .iter()
        .filter(|key| request.get(**key).is_some())
        .count();
        if selector_count > 1 {
            return Err(ScheduleInputError::new(
                "invalid_selector",
                "Exactly one reminder selector is required.",
            )
            .into());
        }
        let title = schedule_title(request.get("title").unwrap_or(&Value::Null))?;
        let id = format!("schedule-{}", Uuid::new_v4());
        let now = (self.now)();
        let record = if request.get("at").is_some() {
            create_at_schedule_record(
                &id,
                request.get("prompt").and_then(Value::as_str).unwrap_or(""),
                &request["at"],
                now,
                &title,
            )?
        } else if request.get("after_seconds").is_some() {
            create_after_schedule_record(
                &id,
                request.get("prompt").and_then(Value::as_str).unwrap_or(""),
                request
                    .get("after_seconds")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                now,
                &title,
            )?
        } else if request.get("every_seconds").is_some() {
            create_every_schedule_record(
                &id,
                request.get("prompt").and_then(Value::as_str).unwrap_or(""),
                request
                    .get("every_seconds")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                now,
                &title,
            )?
        } else if request.get("daily").is_some() {
            create_daily_schedule_record(
                &id,
                request.get("prompt").and_then(Value::as_str).unwrap_or(""),
                &request["daily"],
                now,
                &title,
            )?
        } else if request.get("weekly").is_some() {
            create_weekly_schedule_record(
                &id,
                request.get("prompt").and_then(Value::as_str).unwrap_or(""),
                &request["weekly"],
                now,
                &title,
            )?
        } else if request.get("cron").is_some() {
            create_cron_schedule_record(
                &id,
                request.get("prompt").and_then(Value::as_str).unwrap_or(""),
                &request["cron"],
                now,
                &title,
            )?
        } else {
            return Err(ScheduleInputError::new(
                "invalid_selector",
                "Exactly one reminder selector is required.",
            )
            .into());
        };
        let _chain = self
            .chain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(refusal) = self.reminder_target_refusal(session_id) {
            return Err(refusal.into());
        }
        let task = ScheduleTask {
            session_id: session_id.to_owned(),
            record: record.clone(),
            status: "active".to_owned(),
            last_delivery: None,
            history: Some(DeliveryHistory {
                records: Vec::new(),
                earlier_records_unavailable: false,
                earlier_records_pruned: false,
            }),
        };
        self.seams
            .put_task(&id, &task.to_value())
            .map_err(|error| ScheduleError::Log(ScheduleLogError(error)))?;
        self.seams.emit_changed();
        self.request_drive();
        Ok(record)
    }

    /// Reads the selected session's active tasks in storage order (dsh
    /// `list`).
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    pub(crate) fn list(&self, session_id: &str) -> Result<Vec<ScheduleRecord>, ScheduleLogError> {
        Ok(self
            .tasks()?
            .into_iter()
            .filter(|(_, task)| task.session_id == session_id && task.status == "active")
            .map(|(_, task)| task.record)
            .collect())
    }

    /// Reads all active and inactive reminders with their session
    /// bindings, ordered by target then id (dsh `catalog`).
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    pub(crate) fn catalog(&self) -> Result<Vec<Value>, ScheduleLogError> {
        let mut entries: Vec<Value> = self
            .tasks()?
            .into_iter()
            .map(|(_, task)| {
                let mut entry = task.record.to_value();
                if let Some(object) = entry.as_object_mut() {
                    object.insert("sessionId".to_owned(), json!(task.session_id));
                    object.insert("status".to_owned(), json!(task.status));
                    if let Some(receipt) = &task.last_delivery {
                        object.insert(
                            "lastDelivery".to_owned(),
                            json!({
                                "scheduledAt": receipt.scheduled_at,
                                "deliveredAt": receipt.delivered_at,
                                "messageId": receipt.message_id,
                            }),
                        );
                    }
                }
                entry
            })
            .collect();
        entries.sort_by(|left, right| {
            let left_target = left
                .get("scheduledAt")
                .and_then(Value::as_str)
                .unwrap_or("");
            let right_target = right
                .get("scheduledAt")
                .and_then(Value::as_str)
                .unwrap_or("");
            left_target.cmp(right_target).then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(right.get("id").and_then(Value::as_str).unwrap_or(""))
            })
        });
        Ok(entries)
    }

    /// Deletes one task belonging to the selected session (dsh
    /// `delete`): the row is removed with its saved deliveries.
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    pub(crate) fn delete(&self, session_id: &str, id: &str) -> Result<Value, ScheduleError> {
        let _chain = self
            .chain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self
            .tasks()
            .map_err(ScheduleError::Log)?
            .into_iter()
            .find(|(key, _)| key == id)
            .map(|(_, task)| task);
        match current {
            Some(task) if task.session_id == session_id => {
                self.seams
                    .delete_task(id)
                    .map_err(|error| ScheduleError::Log(ScheduleLogError(error)))?;
                self.seams.emit_changed();
                self.request_drive();
                Ok(json!({ "id": id, "deleted": true }))
            }
            _ => Ok(json!({ "id": id, "deleted": false, "code": "schedule_not_found" })),
        }
    }

    /// Updates one active task in place within the original session
    /// binding (dsh `update`): refusal, binding, ended, then the CAS.
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    pub(crate) fn update(&self, session_id: &str, request: &Value) -> Result<Value, ScheduleError> {
        let _chain = self
            .chain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(refusal) = self.reminder_target_refusal(session_id) {
            return Ok(ScheduleError::Input(refusal).to_error_value());
        }
        let id = request.get("id").and_then(Value::as_str).unwrap_or("");
        let current = self
            .tasks()
            .map_err(ScheduleError::Log)?
            .into_iter()
            .find(|(key, _)| key == id)
            .map(|(_, task)| task);
        let Some(current) = current else {
            return Ok(json!({ "id": id, "updated": false, "code": "schedule_not_found" }));
        };
        if current.session_id != session_id {
            return Ok(json!({ "id": id, "updated": false, "code": "schedule_not_found" }));
        }
        if current.status == "inactive" {
            return Ok(json!({ "id": id, "updated": false, "code": "schedule_ended" }));
        }
        let now = (self.now)();
        let result = resolve_schedule_update(
            &current.record,
            request.get("expected").unwrap_or(&Value::Null),
            request.get("change").filter(|value| !value.is_null()),
            now,
            request,
        );
        let value = match result {
            Ok(record) => {
                let unchanged = record == current.record;
                if !unchanged {
                    let next = ScheduleTask {
                        session_id: current.session_id,
                        record: record.clone(),
                        status: current.status,
                        last_delivery: current.last_delivery,
                        history: current.history,
                    };
                    self.seams
                        .put_task(id, &next.to_value())
                        .map_err(|error| ScheduleError::Log(ScheduleLogError(error)))?;
                    self.seams.emit_changed();
                    self.request_drive();
                }
                json!({ "id": id, "updated": !unchanged, "record": record.to_value() })
            }
            Err(error) => error.to_error_value(),
        };
        Ok(value)
    }

    /// Persists one runtime-driven task transition and publishes the
    /// change notification (the dsh `commit` callback).
    ///
    /// # Errors
    ///
    /// The storage refusal text.
    pub(crate) fn commit_task(&self, task: &ScheduleTask) -> Result<(), ScheduleError> {
        let _chain = self
            .chain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.seams
            .put_task(task.record.id(), &task.to_value())
            .map_err(|error| ScheduleError::Log(ScheduleLogError(error)))?;
        self.seams.emit_changed();
        Ok(())
    }

    /// Requests the nearest obligation recompute (the runtime signal;
    /// absent until the shell spawns the runtime thread).
    pub(crate) fn request_drive(&self) {
        if let Some(signal) = self.signal.get() {
            signal.request();
        }
    }
}

/// Decodes one record value the update reply rides (the caller's own
/// storage round-trip; a decode failure keeps the value form).
pub fn record_from_value(value: &Value) -> Result<ScheduleRecord, ScheduleLogError> {
    super::occurrences::decode_schedule_record(value)
}

/// Names the exact selector property each timing kind must carry (dsh
/// `timingSelector`).
fn timing_selector(kind: &str) -> Option<&'static str> {
    match kind {
        "at" => Some("at"),
        "every" => Some("every_seconds"),
        "daily" => Some("daily"),
        "weekly" => Some("weekly"),
        "cron" => Some("cron"),
        _ => None,
    }
}

/// Validates the exact timing selector keys the update carries (dsh
/// `validateChange`).
///
/// # Errors
///
/// Non-object, unknown discriminant, or wrong key count.
fn validate_change(value: &Value) -> Result<(), ScheduleInputError> {
    if !value.is_object() {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "Timing change must be an object with exactly one supported timing selector.",
        ));
    }
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    let Some(selector) = timing_selector(kind) else {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "Timing change must be an object with exactly one supported timing selector.",
        ));
    };
    let keys: Vec<&str> = value
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if keys.len() != 2 || !keys.contains(&"kind") || !keys.contains(&selector) {
        return Err(ScheduleInputError::new(
            "invalid_rule",
            "Timing change must contain exactly kind and its matching timing selector.",
        ));
    }
    Ok(())
}

/// Applies a replacement name and instruction without touching the rule
/// (dsh `withContent`).
fn with_content(current: &ScheduleRecord, title: String, prompt: String) -> ScheduleRecord {
    if title == current.title() && prompt == current.prompt() {
        return current.clone();
    }
    let mut record = current.clone();
    match &mut record {
        ScheduleRecord::After {
            title: stored,
            prompt: stored_prompt,
            ..
        }
        | ScheduleRecord::At {
            title: stored,
            prompt: stored_prompt,
            ..
        }
        | ScheduleRecord::Every {
            title: stored,
            prompt: stored_prompt,
            ..
        }
        | ScheduleRecord::Daily {
            title: stored,
            prompt: stored_prompt,
            ..
        }
        | ScheduleRecord::Weekly {
            title: stored,
            prompt: stored_prompt,
            ..
        }
        | ScheduleRecord::Cron {
            title: stored,
            prompt: stored_prompt,
            ..
        } => {
            *stored = title;
            *stored_prompt = prompt;
        }
    }
    record
}

/// Resolves the requested rule against the current record after the
/// complete expected record has matched (dsh `changedRecord`): an
/// equivalent normalized timing change keeps the committed target; any
/// other change recomputes exactly as creation computes it.
fn changed_record(
    current: &ScheduleRecord,
    title: String,
    prompt: String,
    change: Option<&Value>,
    now: i64,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let Some(change) = change else {
        return Ok(with_content(current, title, prompt));
    };
    validate_change(change)?;
    match change.get("kind").and_then(Value::as_str).unwrap_or("") {
        "at" => {
            let equivalent = matches!(
                current,
                ScheduleRecord::After { .. } | ScheduleRecord::At { .. }
            ) && super::domain::parse_at_input(&change["at"])
                .is_ok_and(|target| canonical_instant_for_update(target) == current.scheduled_at());
            if equivalent {
                return Ok(with_content(current, title, prompt));
            }
            create_at_schedule_record(current.id(), &prompt, &change["at"], now, &title)
        }
        "every" => {
            let equivalent = matches!(current, ScheduleRecord::Every { .. })
                && change.get("every_seconds").and_then(Value::as_i64)
                    == match current {
                        ScheduleRecord::Every { every_seconds, .. } => Some(*every_seconds),
                        _ => None,
                    };
            if equivalent {
                return Ok(with_content(current, title, prompt));
            }
            create_every_schedule_record(
                current.id(),
                &prompt,
                change
                    .get("every_seconds")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
                now,
                &title,
            )
        }
        "daily" => {
            if let ScheduleRecord::Daily {
                time, time_zone, ..
            } = current
                && let Ok((normalized_time, normalized_zone)) = parse_daily_input(&change["daily"])
                && normalized_time == *time
                && canonicalize_time_zone_name(time_zone)
                    .is_ok_and(|current_zone| current_zone == normalized_zone)
            {
                return Ok(with_content(current, title, prompt));
            }
            create_daily_schedule_record(current.id(), &prompt, &change["daily"], now, &title)
        }
        "weekly" => {
            if let ScheduleRecord::Weekly {
                time,
                time_zone,
                weekdays,
                ..
            } = current
                && let Ok((normalized_time, normalized_zone, normalized_weekdays)) =
                    parse_weekly_input(&change["weekly"])
                && normalized_time == *time
                && canonicalize_time_zone_name(time_zone)
                    .is_ok_and(|current_zone| current_zone == normalized_zone)
                && normalized_weekdays.as_slice() == weekdays.as_slice()
            {
                return Ok(with_content(current, title, prompt));
            }
            create_weekly_schedule_record(current.id(), &prompt, &change["weekly"], now, &title)
        }
        "cron" => {
            if let ScheduleRecord::Cron {
                expression,
                time_zone,
                ..
            } = current
                && let Ok((normalized_expression, normalized_zone)) =
                    parse_cron_input(&change["cron"])
                && normalized_expression == *expression
                && canonicalize_time_zone_name(time_zone)
                    .is_ok_and(|current_zone| current_zone == normalized_zone)
            {
                return Ok(with_content(current, title, prompt));
            }
            create_cron_schedule_record(current.id(), &prompt, &change["cron"], now, &title)
        }
        _ => Err(ScheduleInputError::new(
            "invalid_rule",
            "Timing change must be an object with exactly one supported timing selector.",
        )),
    }
}

/// Formats one resolved at-target for the equivalence comparison (the
/// stored form the `Date.parse(current.scheduledAt)` comparison
/// produces).
fn canonical_instant_for_update(target: i64) -> String {
    super::domain::canonical_instant(target)
}

/// Compares the complete observed record and resolves one update (dsh
/// `resolveScheduleUpdate`).
///
/// # Errors
///
/// The content, timing, or range refusals (returned as the bounded
/// result by the callers).
pub fn resolve_schedule_update(
    current: &ScheduleRecord,
    expected: &Value,
    change: Option<&Value>,
    now: i64,
    content: &Value,
) -> Result<ScheduleRecord, ScheduleError> {
    let decoded = decode_schedule_record(expected).map_err(fixed(ScheduleInputError::new(
        "invalid_rule",
        "expected must be a complete valid Schedule record.",
    )))?;
    if current.to_value() != decoded.to_value() {
        return Err(ScheduleError::UpdateMiss {
            id: current.id().to_owned(),
            code: "schedule_conflict",
        });
    }
    let retained = retained_title(current).map_err(ScheduleError::Log)?;
    let title = match content.get("title") {
        Some(value) if !value.is_null() => schedule_title(value)?,
        _ => retained,
    };
    let prompt = match content.get("prompt") {
        Some(value) if !value.is_null() => schedule_prompt(value.as_str().unwrap_or(""))?,
        _ => current.prompt().to_owned(),
    };
    changed_record(current, title, prompt, change, now).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted seams double: the storage rows live in a map, the
    /// notifications record, and the delivery faces answer (the
    /// job-tools `JobSeams` judgment).
    #[derive(Debug, Default)]
    struct ScriptSeams {
        rows: std::sync::Mutex<std::collections::BTreeMap<String, Value>>,
        changed: std::sync::atomic::AtomicUsize,
        followups: std::sync::Mutex<Vec<String>>,
        flushes: std::sync::Mutex<Vec<String>>,
        depth: u32,
    }

    impl ScriptSeams {
        fn with_depth(depth: u32) -> Self {
            Self {
                depth,
                ..Self::default()
            }
        }
    }

    impl ScheduleSeams for ScriptSeams {
        fn table_entries(&self) -> Result<Vec<(String, Value)>, String> {
            Ok(self
                .rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect())
        }

        fn put_task(&self, id: &str, task: &Value) -> Result<(), String> {
            self.rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.to_owned(), task.clone());
            Ok(())
        }

        fn delete_task(&self, id: &str) -> Result<(), String> {
            self.rows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(id);
            Ok(())
        }

        fn emit_changed(&self) {
            self.changed
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        fn delegation_depth(&self, _session: &str) -> u32 {
            self.depth
        }

        fn followup(&self, session: &str, _message: &Value) -> Result<(), String> {
            self.followups
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(session.to_owned());
            Ok(())
        }

        fn flush(&self, session: &str) -> Result<(), String> {
            self.flushes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(session.to_owned());
            Ok(())
        }
    }

    /// The four management flows over the scripted storage: create
    /// stores the active row and answers the view, list filters by
    /// session, delete removes the bound row, and the CAS update keeps
    /// the id (the four 行为 scenarios' service face).
    #[test]
    fn the_management_flows_drive_the_storage_rows() {
        let seams = ScriptSeams::default();
        let schedules = Schedules::with_clock(
            Box::new(SharedSeams(seams)),
            (30, 200),
            Box::new(|| 1_900_000_000_000),
        );
        let request = json!({
            "title": "Report",
            "prompt": "write the report",
            "after_seconds": 60,
        });
        let record = schedules.create("s1", &request).expect("creates");
        let ScheduleRecord::After { scheduled_at, .. } = &record else {
            panic!("the after rule builds");
        };
        assert_eq!(scheduled_at, "2030-03-17T17:47:40.000Z");
        let listed = schedules.list("s1").expect("lists");
        assert_eq!(listed.len(), 1);
        assert_eq!(schedules.list("s2").expect("lists"), Vec::new());
        let id = record.id().to_owned();
        // The optimistic update keeps the id and rewrites the content.
        let expected = record.to_value();
        let update = json!({
            "id": id,
            "expected": expected,
            "prompt": "rewrite the report",
        });
        let result = schedules.update("s1", &update).expect("updates");
        assert_eq!(result["updated"], json!(true));
        assert_eq!(result["record"]["prompt"], json!("rewrite the report"));
        // A stale expected record conflicts without mutating.
        let stale = json!({
            "id": id,
            "expected": record.to_value(),
            "prompt": "raced",
        });
        let conflict = schedules.update("s1", &stale).expect("updates");
        assert_eq!(conflict["code"], json!("schedule_conflict"));
        // The update path carries no after_seconds (a relative delay
        // requires a new reminder; the tool root refuses it).
        assert_eq!(
            crate::schedule::execute_update(
                &schedules,
                &json!({
                    "id": id,
                    "after_seconds": 60,
                    "prompt": "p",
                }),
                Some("s1")
            )["code"],
            json!("invalid_selector")
        );
        // A fixed-rate change re-anchors from the same queue-time now.
        let record_now = schedules.list("s1").expect("lists").remove(0);
        let every_change = json!({
            "id": id,
            "expected": record_now.to_value(),
            "change": { "kind": "every", "every_seconds": 120 },
        });
        let re_anchored = schedules.update("s1", &every_change).expect("updates");
        assert_eq!(re_anchored["record"]["kind"], json!("every"));
        assert_eq!(
            re_anchored["record"]["scheduledAt"],
            json!("2030-03-17T17:48:40.000Z")
        );
        // A local at in a spring-forward gap refuses; an overlap takes
        // the earlier instant (the creation grammar riding the
        // service).
        let gap = schedules.create(
            "s1",
            &json!({
                "title": "Gap",
                "prompt": "p",
                "at": { "date": "2031-03-09", "time": "02:30:00", "time_zone": "America/New_York" },
            }),
        );
        assert_eq!(
            gap.unwrap_err().to_error_value()["message"],
            json!("The local at time does not exist in the selected time zone.")
        );
        let overlap = schedules
            .create("s1", &json!({
                "title": "Overlap",
                "prompt": "p",
                "at": { "date": "2030-11-03", "time": "01:30:00", "time_zone": "America/New_York" },
            }))
            .expect("the overlap resolves");
        assert_eq!(overlap.scheduled_at(), "2030-11-03T05:30:00.000Z");
        // The recurring decision skips the missed backlog to the latest
        // occurrence (the downtime arm the drive commits).
        let anchor = "2030-01-01T00:00:00.000Z";
        let decision = super::super::domain::decode_instant(anchor).expect("the anchor decodes");
        let occurrence = super::super::occurrences::resolve_every_occurrence(
            3_600,
            anchor,
            decision + 90 * 60_000,
        )
        .expect("resolves");
        assert_eq!(occurrence.occurrence_at, "2030-01-01T01:00:00.000Z");
        let overlap_id = overlap.id().to_owned();
        schedules.delete("s1", &overlap_id).expect("deletes");
        let deleted = schedules.delete("s1", &id).expect("deletes");
        assert_eq!(deleted["deleted"], json!(true));
        assert_eq!(schedules.list("s1").expect("lists"), Vec::new());
        let again = schedules.delete("s1", &id).expect("deletes");
        assert_eq!(again["code"], json!("schedule_not_found"));
    }

    /// A session a delegated child owns refuses creation and updates
    /// with the stable subagent codes (the refusal arms in place; the
    /// batch-0 ruling).
    #[test]
    fn the_subagent_refusals_pin_their_codes() {
        let schedules = Schedules::with_clock(
            Box::new(SharedSeams(ScriptSeams::with_depth(1))),
            (30, 200),
            Box::new(|| 1_900_000_000_000),
        );
        let request = json!({
            "title": "Report",
            "prompt": "write the report",
            "after_seconds": 60,
        });
        assert_eq!(
            schedules
                .create("s1", &request)
                .unwrap_err()
                .to_error_value()["code"],
            json!("subagent_session")
        );
        assert_eq!(
            schedules
                .update("s1", &json!({ "id": "schedule-x" }))
                .expect("updates")["code"],
            json!("subagent_session")
        );
    }

    /// The retention append prunes to the window and the cap, marks the
    /// pruning flags, and keeps the sent prompt (dsh `appendDelivery`).
    #[test]
    fn the_delivery_append_prunes_and_flags() {
        let history = DeliveryHistory {
            records: Vec::new(),
            earlier_records_unavailable: false,
            earlier_records_pruned: false,
        };
        let receipt = DeliveryReceipt {
            scheduled_at: "2030-01-01T00:00:00.000Z".to_owned(),
            delivered_at: "2030-03-01T00:00:00.000Z".to_owned(),
            message_id: "m1".to_owned(),
        };
        let (last, history) =
            append_delivery("write the report", Some(&history), None, receipt, (30, 200));
        assert_eq!(last.message_id, "m1");
        assert_eq!(history.records.len(), 1);
        assert_eq!(
            history.records[0].prompt.as_deref(),
            Some("write the report")
        );
        assert!(!history.earlier_records_unavailable);
        assert!(!history.earlier_records_pruned);
        // The floor reads from the NEW receipt's deliveredAt: an append
        // dated past the window prunes the older records and raises
        // both flags (the dsh `appendDelivery` floor arm).
        let old = DeliveryReceipt {
            scheduled_at: "2030-01-01T00:00:00.000Z".to_owned(),
            delivered_at: "2030-06-01T00:00:00.000Z".to_owned(),
            message_id: "m0".to_owned(),
        };
        let (last, history) = append_delivery(
            "write the report",
            Some(&history),
            Some(&last),
            old,
            (30, 200),
        );
        assert_eq!(last.message_id, "m0");
        // The new receipt itself always survives; the window-dated
        // predecessor prunes.
        assert_eq!(history.records.len(), 1);
        assert_eq!(history.records[0].receipt.message_id, "m0");
        assert!(history.earlier_records_unavailable);
        assert!(history.earlier_records_pruned);
    }

    /// The stored-row decode pins the refinement arms: the
    /// last-delivery-matches-latest check and the unique-message-id
    /// rule (dsh `scheduleTaskSchema`).
    #[test]
    fn the_task_decode_pins_the_refinements() {
        let record = json!({
            "id": "schedule-1",
            "kind": "after",
            "title": "Report",
            "prompt": "write the report",
            "afterSeconds": 60,
            "scheduledAt": "2030-03-13T16:53:20.000Z",
        });
        let receipt = |id: &str| {
            json!({
                "scheduledAt": "2030-03-13T16:53:20.000Z",
                "deliveredAt": "2030-03-13T16:53:21.000Z",
                "messageId": id,
            })
        };
        let task = json!({
            "sessionId": "s1",
            "record": record,
            "status": "active",
            "lastDelivery": receipt("m1"),
            "deliveryHistory": {
                "records": [
                    { "scheduledAt": "2030-03-13T16:53:20.000Z", "deliveredAt": "2030-03-13T16:53:21.000Z", "messageId": "m0" },
                    { "scheduledAt": "2030-03-13T16:53:20.000Z", "deliveredAt": "2030-03-13T16:53:21.000Z", "messageId": "m1" },
                ],
                "earlierRecordsUnavailable": false,
            },
        });
        ScheduleTask::from_value(&task).expect("the row decodes");
        let mismatched = json!({
            "sessionId": "s1",
            "record": task["record"],
            "status": "active",
            "lastDelivery": receipt("m2"),
            "deliveryHistory": task["deliveryHistory"],
        });
        assert_eq!(
            ScheduleTask::from_value(&mismatched).unwrap_err().0,
            "Last delivery must match the latest saved delivery receipt"
        );
    }

    /// A shared handle over the scripted seams (the two-service tests
    /// share one double through an alias).
    struct SharedSeams(ScriptSeams);

    impl ScheduleSeams for SharedSeams {
        fn table_entries(&self) -> Result<Vec<(String, Value)>, String> {
            self.0.table_entries()
        }

        fn put_task(&self, id: &str, task: &Value) -> Result<(), String> {
            self.0.put_task(id, task)
        }

        fn delete_task(&self, id: &str) -> Result<(), String> {
            self.0.delete_task(id)
        }

        fn emit_changed(&self) {
            self.0.emit_changed();
        }

        fn delegation_depth(&self, session: &str) -> u32 {
            self.0.delegation_depth(session)
        }

        fn followup(&self, session: &str, message: &Value) -> Result<(), String> {
            self.0.followup(session, message)
        }

        fn flush(&self, session: &str) -> Result<(), String> {
            self.0.flush(session)
        }
    }

    impl std::fmt::Debug for SharedSeams {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SharedSeams").finish_non_exhaustive()
        }
    }
}
