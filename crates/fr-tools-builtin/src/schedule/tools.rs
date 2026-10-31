// Rust guideline compliant 2026-10-31
//! The four management tool bodies (dsh `tool-schedule/index.ts`): the
//! model-facing validation arms, the service dispatch, the JSON render,
//! and the pending-call presentations.

use serde_json::{Value, json};

use super::domain::{MAX_TITLE_LENGTH, ScheduleError, ScheduleRecord as SuperRecord};
use super::occurrences::{invalid_interval, schedule_view};
use super::service::Schedules;

/// Validates the delegation face of the calling session (dsh
/// `subagentCallerRefusal`; the depth wiring rides the service seams and
/// the M1 composition reports zero until the delegation registry lands).
fn subagent_caller_refusal(schedules: &Schedules, session: Option<&str>) -> Option<Value> {
    let depth = session.map_or(0, |session| schedules.seams().delegation_depth(session));
    (depth > 0).then(|| {
        json!({
            "code": "subagent_session",
            "message": "A delegated subagent cannot use reminders.",
        })
    })
}

/// Maps a service outcome onto the tool value (dsh `operationError`).
fn operation_value(outcome: Result<Value, ScheduleError>) -> Value {
    match outcome {
        Ok(value) => value,
        Err(error) => error.to_error_value(),
    }
}

/// Validates the create selector/content arms the open parameter root
/// cannot express (dsh `validateCreateArgs`).
pub fn validate_create_args(arguments: &Value) -> Option<Value> {
    let selector_count = [
        "after_seconds",
        "at",
        "every_seconds",
        "daily",
        "weekly",
        "cron",
    ]
    .iter()
    .filter(|key| arguments.get(**key).is_some())
    .count();
    let known = [
        "prompt",
        "title",
        "after_seconds",
        "at",
        "every_seconds",
        "daily",
        "weekly",
        "cron",
    ];
    let unknown = arguments
        .as_object()
        .is_some_and(|object| object.keys().any(|key| !known.contains(&key.as_str())));
    if unknown || selector_count != 1 {
        return Some(json!({
            "code": "invalid_selector",
            "message": "schedule_create accepts exactly one of after_seconds, at, every_seconds, daily, weekly, or cron.",
        }));
    }
    let prompt = arguments
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or("");
    if prompt.trim().is_empty() {
        return Some(json!({
            "code": "invalid_prompt",
            "message": "prompt must be non-empty after trimming.",
        }));
    }
    let title = arguments.get("title").and_then(Value::as_str).unwrap_or("");
    if title.trim().is_empty() {
        return Some(json!({
            "code": "invalid_prompt",
            "message": super::domain::REQUIRED_TITLE_MESSAGE,
        }));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the title cap is a validated positive constant"
    )]
    let title_cap = MAX_TITLE_LENGTH as usize;
    if title.trim().encode_utf16().count() > title_cap {
        return Some(json!({
            "code": "invalid_prompt",
            "message": format!("title must be at most {MAX_TITLE_LENGTH} characters."),
        }));
    }
    if arguments.get("after_seconds").is_some() {
        let after = arguments.get("after_seconds");
        let integral = after.and_then(Value::as_i64).is_some();
        let positive = after
            .and_then(Value::as_f64)
            .is_some_and(|value| value > 0.0);
        if !integral || !positive {
            return Some(json!({
                "code": "invalid_rule",
                "message": "after_seconds must be a positive safe integer.",
            }));
        }
    }
    if arguments.get("every_seconds").is_some() {
        let every = arguments.get("every_seconds");
        if every.and_then(Value::as_i64).is_none() {
            return Some(json!({
                "code": "invalid_rule",
                "message": "every_seconds must be a safe integer.",
            }));
        }
        if let Some(error) = invalid_interval(every.and_then(Value::as_i64)).err() {
            return Some(json!({ "code": error.code, "message": error.message }));
        }
    }
    None
}

/// Validates the in-place update's selector count, id, and any supplied
/// content (dsh `validateUpdateArgs`).
pub fn validate_update_args(arguments: &Value) -> Option<Value> {
    let selectors = ["at", "every_seconds", "daily", "weekly", "cron"]
        .iter()
        .filter(|key| arguments.get(**key).is_some())
        .count();
    let known = [
        "id",
        "title",
        "prompt",
        "at",
        "every_seconds",
        "daily",
        "weekly",
        "cron",
    ];
    let unknown = arguments
        .as_object()
        .is_some_and(|object| object.keys().any(|key| !known.contains(&key.as_str())));
    if unknown || selectors > 1 {
        return Some(json!({
            "code": "invalid_selector",
            "message": "schedule_update accepts at most one of at, every_seconds, daily, weekly, or cron.",
        }));
    }
    let id = arguments.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() || id.trim() != id {
        return Some(json!({
            "code": "invalid_rule",
            "message": "schedule_update id must be non-empty without surrounding whitespace.",
        }));
    }
    let title = arguments.get("title").filter(|value| !value.is_null());
    let prompt = arguments.get("prompt").filter(|value| !value.is_null());
    if selectors == 0 && title.is_none() && prompt.is_none() {
        return Some(json!({
            "code": "invalid_selector",
            "message": "schedule_update needs a new title, prompt, or one of at, every_seconds, daily, weekly, or cron.",
        }));
    }
    if let Some(title) = title.and_then(Value::as_str) {
        if title.trim().is_empty() {
            return Some(json!({
                "code": "invalid_prompt",
                "message": super::domain::REQUIRED_TITLE_MESSAGE,
            }));
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the title cap is a validated positive constant"
        )]
        let update_title_cap = MAX_TITLE_LENGTH as usize;
        if title.trim().encode_utf16().count() > update_title_cap {
            return Some(json!({
                "code": "invalid_prompt",
                "message": format!("title must be at most {MAX_TITLE_LENGTH} characters."),
            }));
        }
    }
    if let Some(prompt) = prompt.and_then(Value::as_str)
        && prompt.trim().is_empty()
    {
        return Some(json!({
            "code": "invalid_prompt",
            "message": "prompt must be non-empty after trimming.",
        }));
    }
    if arguments.get("every_seconds").is_some() {
        let every = arguments.get("every_seconds");
        if every.and_then(Value::as_i64).is_none() {
            return Some(json!({
                "code": "invalid_rule",
                "message": "every_seconds must be a safe integer.",
            }));
        }
        if let Some(error) = invalid_interval(every.and_then(Value::as_i64)).err() {
            return Some(json!({ "code": error.code, "message": error.message }));
        }
    }
    None
}

/// The timing replacement the update carries (dsh `timingChangeFrom`).
fn timing_change(arguments: &Value) -> Option<Value> {
    if arguments.get("at").is_some() {
        return Some(json!({ "kind": "at", "at": arguments["at"] }));
    }
    if arguments.get("every_seconds").is_some() {
        return Some(json!({ "kind": "every", "every_seconds": arguments["every_seconds"] }));
    }
    if arguments.get("daily").is_some() {
        return Some(json!({ "kind": "daily", "daily": arguments["daily"] }));
    }
    if arguments.get("weekly").is_some() {
        return Some(json!({ "kind": "weekly", "weekly": arguments["weekly"] }));
    }
    if arguments.get("cron").is_some() {
        return Some(json!({ "kind": "cron", "cron": arguments["cron"] }));
    }
    None
}

/// Executes one `schedule_create` (dsh execute): refusal, validation,
/// then the service create projected onto the view.
pub fn execute_create(schedules: &Schedules, arguments: &Value, session: Option<&str>) -> Value {
    if session.is_none() {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    }
    if let Some(refusal) = subagent_caller_refusal(schedules, session) {
        return refusal;
    }
    if let Some(invalid) = validate_create_args(arguments) {
        return invalid;
    }
    let Some(session) = session else {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    };
    let record = schedules.create(session, arguments);
    match record {
        Ok(record) => schedule_view(&record, super::runtime::current_now_ms()),
        Err(error) => error.to_error_value(),
    }
}

/// Executes one `schedule_list` (dsh execute).
pub fn execute_list(schedules: &Schedules, session: Option<&str>) -> Value {
    if session.is_none() {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    }
    if let Some(refusal) = subagent_caller_refusal(schedules, session) {
        return refusal;
    }
    let Some(session) = session else {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    };
    let now = super::runtime::current_now_ms();
    match schedules.list(session) {
        Ok(records) => json!(
            records
                .iter()
                .map(|record| schedule_view(record, now))
                .collect::<Vec<_>>()
        ),
        Err(error) => operation_value(Err(ScheduleError::Log(error))),
    }
}

/// Executes one `schedule_delete` (dsh execute): the id check precedes
/// the agent arms.
pub fn execute_delete(schedules: &Schedules, arguments: &Value, session: Option<&str>) -> Value {
    let id = arguments.get("id").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() || id.trim() != id {
        return json!({
            "code": "invalid_rule",
            "message": "schedule_delete id must be non-empty without surrounding whitespace.",
        });
    }
    if session.is_none() {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    }
    if let Some(refusal) = subagent_caller_refusal(schedules, session) {
        return refusal;
    }
    let Some(session) = session else {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    };
    operation_value(schedules.delete(session, id))
}

/// Executes one `schedule_update` (dsh execute): the list-then-catalog
/// miss arms, then the CAS.
pub fn execute_update(schedules: &Schedules, arguments: &Value, session: Option<&str>) -> Value {
    if session.is_none() {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    }
    if let Some(refusal) = subagent_caller_refusal(schedules, session) {
        return refusal;
    }
    if let Some(invalid) = validate_update_args(arguments) {
        return invalid;
    }
    let Some(session) = session else {
        return json!({ "code": "internal_error", "message": "The schedule operation failed." });
    };
    let id = arguments.get("id").and_then(Value::as_str).unwrap_or("");
    let listed = match schedules.list(session) {
        Ok(records) => records,
        Err(error) => return operation_value(Err(ScheduleError::Log(error))),
    };
    if !listed.iter().any(|record| record.id() == id) {
        let ended = schedules.catalog().is_ok_and(|entries| {
            entries.iter().any(|entry| {
                entry.get("sessionId").and_then(Value::as_str) == Some(session)
                    && entry.get("id").and_then(Value::as_str) == Some(id)
            })
        });
        return json!({
            "id": id,
            "updated": false,
            "code": if ended { "schedule_ended" } else { "schedule_not_found" },
        });
    }
    let mut request = json!({
        "id": id,
        "expected": listed
            .iter()
            .find(|record| record.id() == id)
            .map(SuperRecord::to_value),
    });
    if let Some(change) = timing_change(arguments) {
        request["change"] = change;
    }
    if arguments.get("title").is_some() {
        request["title"] = arguments["title"].clone();
    }
    if arguments.get("prompt").is_some() {
        request["prompt"] = arguments["prompt"].clone();
    }
    match schedules.update(session, &request) {
        Ok(value) => {
            if value.get("updated").and_then(Value::as_bool) == Some(true) {
                match value.get("record").map(super::service::record_from_value) {
                    Some(Ok(record)) => schedule_view(&record, super::runtime::current_now_ms()),
                    _ => value,
                }
            } else {
                value
            }
        }
        Err(error) => error.to_error_value(),
    }
}

/// Renders one canonical output value onto the model-facing text block
/// (dsh `renderValue`: the `JSON.stringify` of the view or error).
pub fn render(_arguments: &Value, value: &Value) -> Vec<Value> {
    vec![json!({ "type": "text", "text": value.to_string() })]
}

/// The generic pending presentation (dsh `present`).
fn present(title: &str, kind: &str, raw_input: Option<&Value>) -> Value {
    let mut view = json!({ "card": "generic", "title": title, "kind": kind });
    if let Some(raw_input) = raw_input {
        view["rawInput"] = raw_input.clone();
    }
    view
}

/// The `schedule_create` pending card (dsh `presentCall`).
pub fn present_call_create(arguments: &Value) -> Value {
    present("Create reminder", "other", arguments.get("prompt"))
}

/// The `schedule_list` pending card (dsh `presentCall`).
pub fn present_call_list(_arguments: &Value) -> Value {
    present("List reminders", "read", None)
}

/// The `schedule_delete` pending card (dsh `presentCall`).
pub fn present_call_delete(arguments: &Value) -> Value {
    present("Delete reminder", "other", arguments.get("id"))
}

/// The `schedule_update` pending card (dsh `presentCall`).
pub fn present_call_update(arguments: &Value) -> Value {
    present("Update reminder", "other", arguments.get("id"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tool-surface validation arms pin their verbatim refusals
    /// before any service dispatch (the four tools' 越界拒绝 face; the
    /// dsh `validateCreateArgs`/`validateUpdateArgs` texts).
    #[test]
    fn the_tool_validations_pin_the_refusal_arms() {
        let schedules = crate::schedule::Schedules::new(Box::new(RefusingSeams));
        // create: the selector count, the unknown key, and the
        // content/interval arms.
        let multi = json!({ "prompt": "p", "title": "t", "after_seconds": 1, "at": "2031-01-01T00:00:00Z" });
        assert_eq!(
            validate_create_args(&multi).expect("refuses")["message"],
            json!(
                "schedule_create accepts exactly one of after_seconds, at, every_seconds, daily, weekly, or cron."
            )
        );
        let zero = json!({ "prompt": "p", "title": "t", "after_seconds": 0 });
        assert_eq!(
            validate_create_args(&zero).expect("refuses")["message"],
            json!("after_seconds must be a positive safe integer.")
        );
        let fast = json!({ "prompt": "p", "title": "t", "every_seconds": 59 });
        assert_eq!(
            validate_create_args(&fast).expect("refuses")["code"],
            json!("frequency_too_high")
        );
        // update: the id whitespace arm and the no-op arm.
        let spaced = json!({ "id": " schedule-1 ", "prompt": "p" });
        assert_eq!(
            validate_update_args(&spaced).expect("refuses")["message"],
            json!("schedule_update id must be non-empty without surrounding whitespace.")
        );
        let noop = json!({ "id": "schedule-1" });
        assert_eq!(
            validate_update_args(&noop).expect("refuses")["code"],
            json!("invalid_selector")
        );
        // The tool bodies refuse a caller without a session (the dsh
        // `agent === undefined` arm) before any dispatch.
        let arguments = json!({ "prompt": "p", "title": "t", "after_seconds": 60 });
        assert_eq!(
            execute_create(&schedules, &arguments, None)["code"],
            json!("internal_error")
        );
        assert_eq!(
            execute_list(&schedules, None)["code"],
            json!("internal_error")
        );
    }

    /// The refusing double (the bodies never reach a service call in
    /// this test).
    #[derive(Debug)]
    struct RefusingSeams;

    impl crate::schedule::ScheduleSeams for RefusingSeams {
        fn table_entries(&self) -> Result<Vec<(String, Value)>, String> {
            Ok(Vec::new())
        }

        fn put_task(&self, _id: &str, _task: &Value) -> Result<(), String> {
            Err("not expected".to_owned())
        }

        fn delete_task(&self, _id: &str) -> Result<(), String> {
            Err("not expected".to_owned())
        }

        fn emit_changed(&self) {}

        fn delegation_depth(&self, _session: &str) -> u32 {
            0
        }

        fn followup(&self, _session: &str, _message: &Value) -> Result<(), String> {
            Err("not expected".to_owned())
        }

        fn flush(&self, _session: &str) -> Result<(), String> {
            Err("not expected".to_owned())
        }
    }

    /// The render keeps the JSON body verbatim and the pending cards
    /// pin the four titles with their kinds and raw inputs (the dsh
    /// `renderValue`/`present` pair).
    #[test]
    fn the_render_and_presentations_pin_the_card_family() {
        let value = json!({ "id": "schedule-1", "state": "scheduled" });
        let rendered = render(&Value::Null, &value);
        assert_eq!(
            rendered,
            vec![json!({ "type": "text", "text": r#"{"id":"schedule-1","state":"scheduled"}"# })]
        );
        assert_eq!(
            present_call_create(&json!({ "prompt": "hello" })),
            json!({ "card": "generic", "title": "Create reminder", "kind": "other", "rawInput": "hello" })
        );
        assert_eq!(
            present_call_list(&Value::Null),
            json!({ "card": "generic", "title": "List reminders", "kind": "read" })
        );
        assert_eq!(
            present_call_delete(&json!({ "id": "schedule-9" })),
            json!({ "card": "generic", "title": "Delete reminder", "kind": "other", "rawInput": "schedule-9" })
        );
        assert_eq!(
            present_call_update(&json!({ "id": "schedule-9" })),
            json!({ "card": "generic", "title": "Update reminder", "kind": "other", "rawInput": "schedule-9" })
        );
    }
}
