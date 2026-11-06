// Rust guideline compliant 2026-11-06
//! The durable child descriptor (dsh `subagent/src/descriptor.ts` 1:1):
//! the versioned `subagent/descriptor` event that identifies every
//! session-backed subagent, snapshots its explicit composition fields,
//! and classifies cold-resume support.
//!
//! The descriptor deliberately snapshots explicit fields rather than a
//! merge-extensible options object: an unrelated extension value cannot
//! make continuation fail merely because it is not JSON, and later
//! composition inputs require a deliberate version change.

use serde_json::{Map, Value, json};

/// The current descriptor format version (dsh `SUBAGENT_DESCRIPTOR_VERSION`).
pub const DESCRIPTOR_VERSION: u64 = 3;

/// The discriminated durable mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A terminal one-shot run.
    OneShot,
    /// A resumable conversation.
    Continuable,
}

impl Mode {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "one-shot",
            Self::Continuable => "continuable",
        }
    }
}

/// One continuable descriptor's composition fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ContinuableDescriptor {
    /// The establishing provider name.
    pub provider: String,
    /// The durable creation label.
    pub label: String,
    /// The resolved child provider route, when one was declared.
    pub agent_provider: Option<String>,
    /// The resolved child model route, when one was declared.
    pub agent_model: Option<String>,
    /// The resolved child reasoning effort, when one was declared.
    pub agent_reasoning_effort: Option<String>,
    /// A per-child persona shadowing the deployment persona on resume.
    pub persona: Option<String>,
    /// A child tool restriction reapplied on resume.
    pub tool_filter: Option<Value>,
}

impl ContinuableDescriptor {
    /// The exact declared key set (dsh `CONTINUABLE_DESCRIPTOR_KEYS`).
    fn known_keys() -> &'static [&'static str] {
        &[
            "version",
            "mode",
            "provider",
            "label",
            "agentProvider",
            "agentModel",
            "agentReasoningEffort",
            "persona",
            "toolFilter",
        ]
    }
}

/// Stamps the versioned continuable payload (dsh
/// `snapshotSubagentDescriptor`): explicit fields only, in the dsh
/// key order, with absent optional fields omitted.
#[must_use]
pub fn snapshot(input: &ContinuableDescriptor) -> Value {
    let mut data = Map::new();
    data.insert("version".to_owned(), json!(DESCRIPTOR_VERSION));
    data.insert("mode".to_owned(), json!("continuable"));
    data.insert("provider".to_owned(), json!(input.provider));
    data.insert("label".to_owned(), json!(input.label));
    if let Some(provider) = &input.agent_provider {
        data.insert("agentProvider".to_owned(), json!(provider));
    }
    if let Some(model) = &input.agent_model {
        data.insert("agentModel".to_owned(), json!(model));
    }
    if let Some(effort) = &input.agent_reasoning_effort {
        data.insert("agentReasoningEffort".to_owned(), json!(effort));
    }
    if let Some(persona) = &input.persona {
        data.insert("persona".to_owned(), json!(persona));
    }
    if let Some(tool_filter) = &input.tool_filter {
        data.insert("toolFilter".to_owned(), tool_filter.clone());
    }
    Value::Object(data)
}

/// The parsed descriptor fold product.
#[derive(Debug, Clone, PartialEq)]
pub enum FoldedDescriptor {
    /// A current-version continuable descriptor.
    Continuable(ContinuableDescriptor),
    /// A terminal one-shot record (the label may be absent).
    OneShot {
        /// The establishing provider name.
        provider: String,
        /// The optional durable creation label.
        label: Option<String>,
    },
}

fn parse_tool_filter(value: &Value) -> Result<Value, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "persisted subagent descriptor toolFilter must be an object".to_owned())?;
    let known = ["allow", "deny"];
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            return Err(format!(
                "persisted subagent descriptor toolFilter has unknown field \"{key}\""
            ));
        }
    }
    let allow = object.get("allow");
    let deny = object.get("deny");
    if allow.is_none() && deny.is_none() {
        return Err(
            "persisted subagent descriptor toolFilter must declare allow and/or deny".to_owned(),
        );
    }
    for field in [allow, deny].into_iter().flatten() {
        let items = field.as_array().ok_or_else(|| {
            "persisted subagent descriptor toolFilter field must be an array of strings".to_owned()
        })?;
        for item in items {
            if !item.is_string() {
                return Err(
                    "persisted subagent descriptor toolFilter field must be an array of strings"
                        .to_owned(),
                );
            }
        }
    }
    Ok(value.clone())
}

/// Validates one persisted payload for the current runtime (dsh
/// `parseSubagentDescriptor`).
///
/// # Errors
///
/// Returns the shape refusal when a current-version payload does not
/// match its complete declared schema.
pub fn parse(data: &Value) -> Result<Option<FoldedDescriptor>, String> {
    let object = data
        .as_object()
        .ok_or_else(|| "persisted subagent descriptor payload must be an object".to_owned())?;
    let version = data
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| "persisted subagent descriptor version must be a number".to_owned())?;
    if version != DESCRIPTOR_VERSION {
        return Ok(None);
    }
    let mode = data.get("mode").and_then(Value::as_str).ok_or_else(|| {
        "persisted subagent descriptor mode must be \"one-shot\" or \"continuable\"".to_owned()
    })?;
    let known: &[&str] = if mode == "one-shot" {
        &["version", "mode", "provider", "label"]
    } else if mode == "continuable" {
        ContinuableDescriptor::known_keys()
    } else {
        return Err(
            "persisted subagent descriptor mode must be \"one-shot\" or \"continuable\"".to_owned(),
        );
    };
    for key in object.keys() {
        if !known.contains(&key.as_str()) {
            return Err(format!(
                "persisted subagent descriptor payload has unknown field \"{key}\""
            ));
        }
    }
    let provider = data
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| "persisted subagent descriptor provider must be a string".to_owned())?
        .to_owned();
    let optional_string = |key: &str| -> Result<Option<String>, String> {
        match data.get(key) {
            None => Ok(None),
            Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_str()
                .map(str::to_owned)
                .map(Some)
                .ok_or_else(|| format!("persisted subagent descriptor {key} must be a string")),
        }
    };
    if mode == "one-shot" {
        return Ok(Some(FoldedDescriptor::OneShot {
            provider,
            label: optional_string("label")?,
        }));
    }
    let label = data
        .get("label")
        .and_then(Value::as_str)
        .ok_or_else(|| "persisted subagent descriptor label must be a string".to_owned())?
        .to_owned();
    let tool_filter = match data.get("toolFilter") {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_tool_filter(value)?),
    };
    Ok(Some(FoldedDescriptor::Continuable(ContinuableDescriptor {
        provider,
        label,
        agent_provider: optional_string("agentProvider")?,
        agent_model: optional_string("agentModel")?,
        agent_reasoning_effort: optional_string("agentReasoningEffort")?,
        persona: optional_string("persona")?,
        tool_filter,
    })))
}

/// Folds one child log to its first `subagent/descriptor` event (dsh
/// `foldSubagentDescriptor`): the first event is authoritative — the
/// establishing provider appends exactly one.
///
/// # Errors
///
/// Returns the shape refusal a current-version payload raises.
pub fn fold(events: &[Value]) -> Result<Option<FoldedDescriptor>, String> {
    for event in events {
        if event.get("type").and_then(Value::as_str) == Some(crate::wire::DESCRIPTOR_EVENT) {
            return parse(&event.get("data").cloned().unwrap_or(Value::Null));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::{ContinuableDescriptor, DESCRIPTOR_VERSION, Mode, fold, snapshot};
    use serde_json::{Value, json};

    fn descriptor() -> ContinuableDescriptor {
        ContinuableDescriptor {
            provider: "spawn".to_owned(),
            label: "research".to_owned(),
            agent_provider: Some("deepseek-official".to_owned()),
            agent_model: None,
            agent_reasoning_effort: Some("low".to_owned()),
            persona: None,
            tool_filter: None,
        }
    }

    #[test]
    fn the_snapshot_stamps_version_and_omits_absent_fields() {
        let value = snapshot(&descriptor());
        assert_eq!(value["version"], json!(DESCRIPTOR_VERSION));
        assert_eq!(value["mode"], json!(Mode::Continuable.as_str()));
        assert_eq!(value["provider"], json!("spawn"));
        assert_eq!(value["label"], json!("research"));
        assert_eq!(value["agentProvider"], json!("deepseek-official"));
        assert_eq!(value["agentReasoningEffort"], json!("low"));
        assert!(
            !value
                .as_object()
                .expect("object")
                .contains_key("agentModel")
        );
        assert!(!value.as_object().expect("object").contains_key("persona"));
        assert!(
            !value
                .as_object()
                .expect("object")
                .contains_key("toolFilter")
        );
    }

    #[test]
    fn the_fold_round_trips_the_snapshot() {
        let value = snapshot(&descriptor());
        let events = [json!({
            "type": crate::wire::DESCRIPTOR_EVENT,
            "seq": 3,
            "time": 1,
            "data": value,
        })];
        let folded = fold(&events).expect("fold").expect("some");
        let super::FoldedDescriptor::Continuable(parsed) = folded else {
            panic!("continuable");
        };
        assert_eq!(parsed, descriptor());
    }

    #[test]
    fn the_fold_skips_unrelated_events_and_reads_the_first_descriptor() {
        let first = snapshot(&descriptor());
        let mut second = descriptor();
        second.label = "rewritten".to_owned();
        let events = [
            json!({ "type": "turn/start", "seq": 0 }),
            json!({ "type": crate::wire::DESCRIPTOR_EVENT, "seq": 1, "data": first }),
            json!({ "type": crate::wire::DESCRIPTOR_EVENT, "seq": 2, "data": snapshot(&second) }),
        ];
        let folded = fold(&events).expect("fold").expect("some");
        let super::FoldedDescriptor::Continuable(parsed) = folded else {
            panic!("continuable");
        };
        assert_eq!(parsed.label, "research");
    }

    #[test]
    fn an_unknown_version_folds_to_none() {
        let mut value = snapshot(&descriptor());
        value["version"] = json!(2);
        let events = [json!({ "type": crate::wire::DESCRIPTOR_EVENT, "data": value })];
        assert!(fold(&events).expect("fold").is_none());
    }

    #[test]
    fn an_unknown_current_version_field_refuses() {
        let mut value = snapshot(&descriptor());
        value
            .as_object_mut()
            .expect("object")
            .insert("mystery".to_owned(), json!(1));
        let events = [json!({ "type": crate::wire::DESCRIPTOR_EVENT, "data": value })];
        let error = fold(&events).expect_err("the unknown field refuses");
        assert!(
            error.contains("unknown field \"mystery\""),
            "names the field: {error}"
        );
    }

    #[test]
    fn a_tool_filter_requires_allow_or_deny() {
        let mut input = descriptor();
        input.tool_filter = Some(json!({}));
        let events = [json!({ "type": crate::wire::DESCRIPTOR_EVENT, "data": snapshot(&input) })];
        let error = fold(&events).expect_err("the empty filter refuses");
        assert!(error.contains("must declare allow and/or deny"), "{error}");

        input.tool_filter = Some(json!({ "deny": ["bash"] }));
        let events = [json!({ "type": crate::wire::DESCRIPTOR_EVENT, "data": snapshot(&input) })];
        let folded = fold(&events).expect("fold").expect("some");
        let super::FoldedDescriptor::Continuable(parsed) = folded else {
            panic!("continuable");
        };
        assert_eq!(parsed.tool_filter, Some(json!({ "deny": ["bash"] })));
    }

    #[test]
    fn the_one_shot_arm_keeps_the_optional_label_absent() {
        let value: Value = json!({
            "version": DESCRIPTOR_VERSION,
            "mode": "one-shot",
            "provider": "acp",
        });
        let events = [json!({ "type": crate::wire::DESCRIPTOR_EVENT, "data": value })];
        let folded = fold(&events).expect("fold").expect("some");
        let super::FoldedDescriptor::OneShot { provider, label } = folded else {
            panic!("one-shot");
        };
        assert_eq!(provider, "acp");
        assert_eq!(label, None);
    }
}
