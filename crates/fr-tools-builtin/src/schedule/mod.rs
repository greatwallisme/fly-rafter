// Rust guideline compliant 2026-10-31
//! The schedule domain (dsh `@deepseek-ai/dsh-schedule`,
//! module-for-module; the fr-storage directory-module judgment shape).
//!
//! The legacy `schedule/change` fold face is structurally unreachable in
//! M1 (the Host path writes no `schedule/change` session events and no
//! `session/created` event exists on the kernel bus; the batch-0
//! judgment family), so only the Host storage decode half of the
//! `decodeScheduleRecord` legacy arms is ported. The delivery-history
//! read page and the archive admission have no M1 consumer either.

mod domain;
mod occurrences;
mod runtime;
mod service;
mod tools;

pub(crate) use runtime::{RuntimeSignal, spawn as spawn_runtime};
pub use service::{
    DOMAIN_NAME, DOMAIN_TABLE, DOMAIN_VERSION, ScheduleSeams, ScheduleTask, Schedules,
};
pub(crate) use tools::{
    execute_create, execute_delete, execute_list, execute_update, present_call_create,
    present_call_delete, present_call_list, present_call_update, render,
};

/// Validates one stored task row (the open gate's decode arm).
pub(crate) fn validate_task_row(
    row: &serde_json::Value,
) -> Result<ScheduleTask, domain::ScheduleLogError> {
    ScheduleTask::from_value(row)
}

/// The recompute signal constructor (the shell binds it before any
/// caller can observe the service).
pub(crate) fn runtime_handle() -> std::sync::Arc<RuntimeSignal> {
    std::sync::Arc::new(RuntimeSignal::default())
}
