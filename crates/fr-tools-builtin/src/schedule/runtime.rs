// Rust guideline compliant 2026-10-31
//! The delivery runtime (dsh `runtime.ts`): one plugin-owned timer
//! thread over the stored tasks, the per-drive delivery with its
//! recurring batching, and the durable commit after the acknowledged
//! inbox splice.

use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use super::domain::{MAX_TIMER_DELAY_MS, ScheduleRecord};
use super::domain::{ScheduleError, ScheduleLogError, fixed};
use super::occurrences::{
    DeliveryReceipt, append_delivery, render_recurring_reminder_batch_framing,
    render_reminder_framing, resolve_recurring_occurrence,
};
use super::service::{ScheduleTask, Schedules};

/// The recompute signal the service mutations raise and the timer
/// thread waits on (dsh `requestDrive` + `setTimeout`).
#[derive(Debug, Default)]
pub struct RuntimeSignal {
    state: std::sync::Mutex<SignalState>,
    wake: std::sync::Condvar,
}

#[derive(Debug, Default)]
struct SignalState {
    requested: bool,
    deadline_changed: bool,
}

impl RuntimeSignal {
    /// Requests one recompute (dsh `requestDrive`).
    pub fn request(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.requested = true;
        state.deadline_changed = true;
        drop(state);
        self.wake.notify_all();
    }

    /// Re-arms the wait deadline (the drive's timer leg).
    fn rearm(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.deadline_changed = true;
        drop(state);
        self.wake.notify_all();
    }

    /// Waits for the next obligation: a mutation request or deadline
    /// hit drives; a re-arm loop recomputes the nearest target first.
    fn wait(&self, deadline: Option<std::time::Instant>) -> Wait {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let requested = state.requested;
            let changed = state.deadline_changed;
            state.requested = false;
            state.deadline_changed = false;
            if requested {
                return Wait::Drive;
            }
            if changed {
                return Wait::Rearm;
            }
            match deadline {
                Some(deadline) => {
                    let timeout = std::time::Instant::now().saturating_duration_since(deadline);
                    let (guard, result) = self
                        .wake
                        .wait_timeout_while(state, timeout, |state| {
                            !(state.requested || state.deadline_changed)
                        })
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state = guard;
                    if result.timed_out() {
                        return Wait::Drive;
                    }
                }
                None => {
                    state = self
                        .wake
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

/// The wait outcome (drive on a request or a passed deadline; re-arm on
/// a deadline change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// A mutation requested a recompute or the deadline passed.
    Drive,
    /// The nearest target changed; recompute the wait first.
    Rearm,
}

/// Spawns the delivery runtime over the shared service and signal (the
/// plugin-owned thread: an empty require stack, joined before teardown —
/// the notice-worker pump discipline).
pub fn spawn(schedules: Arc<Schedules>, signal: Arc<RuntimeSignal>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("fr-tools-builtin-schedule".to_owned())
        .spawn(move || {
            loop {
                let deadline = next_deadline(&schedules);
                match signal.wait(deadline) {
                    Wait::Rearm => continue,
                    Wait::Drive => drive(&schedules),
                }
                signal.rearm();
            }
        })
        .expect("the runtime thread spawns")
}

/// The nearest active target, clamped into the timer range (dsh drive's
/// timer leg; `None` parks the wait until a mutation arrives).
fn next_deadline(schedules: &Schedules) -> Option<std::time::Instant> {
    let now = current_now();
    let next = schedules
        .tasks()
        .ok()?
        .into_iter()
        .filter(|(_, task)| task.status == "active")
        .filter_map(|(_, task)| {
            super::domain::decode_instant(task.record.scheduled_at())
                .ok()
                .map(|target| target.saturating_sub(now).clamp(0, MAX_TIMER_DELAY_MS))
        })
        .min()?;
    Some(std::time::Instant::now() + std::time::Duration::from_millis(u64::try_from(next).ok()?))
}

/// The wall-clock sample the tool surface reads (epoch milliseconds).
pub fn current_now_ms() -> i64 {
    current_now()
}

/// The wall-clock sample (epoch milliseconds).
fn current_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// One admitted delivery: the due task with its resolved occurrence.
struct Admitted {
    task: ScheduleTask,
    occurrence_at: String,
    next_scheduled_at: Option<String>,
}

/// Runs one delivery pass (dsh `drive`): the due tasks, the recurring
/// same-session batches, the acknowledged splice, and the durable
/// commits.
fn drive(schedules: &Schedules) {
    let mut failed: Vec<String> = Vec::new();
    let mut handled: Vec<String> = Vec::new();
    let scan_now = current_now();
    let Ok(tasks) = schedules.tasks() else {
        return;
    };
    let due: Vec<ScheduleTask> = tasks
        .into_iter()
        .filter(|(_, task)| task.status == "active")
        .map(|(_, task)| task)
        .filter(|task| {
            super::domain::decode_instant(task.record.scheduled_at())
                .is_ok_and(|target| target <= scan_now)
        })
        .collect();
    for task in &due {
        if handled.contains(&task.record.id().to_owned()) {
            continue;
        }
        let group: Vec<ScheduleTask> = if task.record.is_recurring() {
            due.iter()
                .filter(|candidate| {
                    candidate.session_id == task.session_id && candidate.record.is_recurring()
                })
                .cloned()
                .collect()
        } else {
            vec![task.clone()]
        };
        for member in &group {
            handled.push(member.record.id().to_owned());
        }
        deliver_group(schedules, &group, &mut failed);
    }
    let _ = failed;
}

/// Delivers one admitted group and commits its members (the dsh per-
/// group try arm, including the not-acknowledged failure ledger).
#[expect(
    clippy::too_many_lines,
    reason = "the delivery mirrors the dsh try arm one statement per step; splitting it would hide the commit ordering"
)]
fn deliver_group(schedules: &Schedules, group: &[ScheduleTask], failed: &mut Vec<String>) {
    let now = current_now();
    // Session restoration can span a wall-clock rollback; future members
    // keep their timer obligation (M1 admits the group as restored).
    let admitted: Vec<ScheduleTask> = group
        .iter()
        .filter(|member| {
            super::domain::decode_instant(member.record.scheduled_at())
                .is_ok_and(|target| target <= now)
        })
        .cloned()
        .collect();
    if admitted.is_empty() {
        return;
    }
    let recurring: Vec<Admitted> = admitted
        .iter()
        .filter(|member| member.record.is_recurring())
        .filter_map(
            |member| match resolve_recurring_occurrence(&member.record, now) {
                Ok(occurrence) => Some(Admitted {
                    task: member.clone(),
                    occurrence_at: occurrence.occurrence_at,
                    next_scheduled_at: occurrence.next_scheduled_at,
                }),
                Err(_) => None,
            },
        )
        .collect();
    let one_shot = !admitted[0].record.is_recurring();
    let text = if one_shot {
        render_reminder_framing(&admitted[0].record)
    } else {
        let rows: Vec<(&ScheduleRecord, &str)> = recurring
            .iter()
            .map(|member| (&member.task.record, member.occurrence_at.as_str()))
            .collect();
        render_recurring_reminder_batch_framing(&rows)
    };
    let message = json!({
        "role": "user",
        "id": Uuid::new_v4().to_string(),
        "content": [{ "type": "text", "text": text }],
        "source": { "kind": "schedule" },
    });
    let seams = schedules.seams();
    let delivered = (|| -> Result<(), ScheduleError> {
        seams
            .followup(&admitted[0].session_id, &message)
            .map_err(|error| ScheduleError::Log(ScheduleLogError(error)))?;
        seams
            .flush(&admitted[0].session_id)
            // The refusal text is fixed: the dsh arm discards the
            // transport cause.
            .map_err(fixed(super::domain::ScheduleError::Log(
                super::domain::ScheduleLogError(
                    "Session persistence did not acknowledge the reminder".to_owned(),
                ),
            )))?;
        Ok(())
    })();
    match delivered {
        Ok(()) => {
            let delivered_at = super::domain::canonical_instant(current_now());
            let message_id = message
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if one_shot {
                commit_member(
                    schedules,
                    &admitted[0],
                    None,
                    None,
                    &delivered_at,
                    &message_id,
                    false,
                );
            }
            for member in &recurring {
                commit_member(
                    schedules,
                    &member.task,
                    Some(&member.occurrence_at),
                    member.next_scheduled_at.as_deref(),
                    &delivered_at,
                    &message_id,
                    true,
                );
            }
        }
        Err(error) => {
            let failed_at = current_now();
            for member in &admitted {
                if super::domain::decode_instant(member.record.scheduled_at())
                    .is_ok_and(|target| target <= failed_at)
                {
                    failed.push(member.record.id().to_owned());
                }
            }
            let ids = admitted
                .iter()
                .map(|member| json!(member.record.id()))
                .collect::<Vec<_>>();
            eprintln!(
                "schedule: reminders {} were not acknowledged: {error}",
                serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_owned())
            );
        }
    }
}

/// Commits one member's post-delivery state (dsh commit arms: the
/// one-shot deactivates, the recurring advances to the next target).
fn commit_member(
    schedules: &Schedules,
    task: &ScheduleTask,
    occurrence_at: Option<&str>,
    next_scheduled_at: Option<&str>,
    delivered_at: &str,
    message_id: &str,
    recurring: bool,
) -> bool {
    let mut next = task.clone();
    if recurring {
        if let Some(next_target) = next_scheduled_at {
            set_scheduled_at(&mut next.record, next_target);
            next.status = "active".to_string();
        } else {
            if let Some(occurrence_at) = occurrence_at {
                set_scheduled_at(&mut next.record, occurrence_at);
            }
            next.status = "inactive".to_string();
        }
    } else {
        next.status = "inactive".to_string();
    }
    let receipt = DeliveryReceipt {
        scheduled_at: occurrence_at
            .unwrap_or_else(|| task.record.scheduled_at())
            .to_owned(),
        delivered_at: delivered_at.to_owned(),
        message_id: message_id.to_owned(),
    };
    let (last, history) = append_delivery(
        task.record.prompt(),
        task.history.as_ref(),
        task.last_delivery.as_ref(),
        receipt,
        schedules.retention(),
    );
    next.last_delivery = Some(last);
    next.history = Some(history);
    schedules.commit_task(&next).is_ok()
}

/// Rewrites the committed target of one record copy (the runtime's
/// advance).
fn set_scheduled_at(record: &mut ScheduleRecord, target: &str) {
    match record {
        ScheduleRecord::After { scheduled_at, .. }
        | ScheduleRecord::At { scheduled_at, .. }
        | ScheduleRecord::Every { scheduled_at, .. }
        | ScheduleRecord::Daily { scheduled_at, .. }
        | ScheduleRecord::Weekly { scheduled_at, .. }
        | ScheduleRecord::Cron { scheduled_at, .. } => {
            *scheduled_at = target.to_string();
        }
    }
}
