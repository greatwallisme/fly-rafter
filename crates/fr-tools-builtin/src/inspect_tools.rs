// Rust guideline compliant 2026-11-01
//! The Cordis inspect pair (dsh `tool-cordis`): the read-only runtime
//! API discovery tools over the inspect registry.
//!
//! The fr registry is the Q16f-B honest face: an empty provider table
//! that never fabricates entries, so `list` always reports no
//! providers and a `query` always refuses through the dsh
//! not-registered texts (the dsh base bundle mounts neither tool —
//! the baked gate stays off).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::tools::BodyError;

/// The baked gate: the dsh base bundle mounts neither `tool-cordis`
/// tool (web-app/desktop compositions only — the Q16f-B baked-off
/// ruling).
pub const BAKED_CORDIS_INSPECT: bool = false;

/// The inspect registry (dsh `cordisInspect`): the provider table
/// keyed by manifest id. The fr composition registers no providers in
/// M1, so the table stays empty and every lookup refuses honestly.
#[derive(Debug, Default)]
pub struct InspectRegistry {
    providers: BTreeMap<String, Value>,
}

impl InspectRegistry {
    /// The empty registry (the M1 honest face).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lists the registered provider manifests (dsh `list`): the
    /// id-ordered snapshot; empty in the fr composition.
    #[must_use]
    pub fn list(&self) -> Vec<Value> {
        self.providers.values().cloned().collect()
    }

    /// Runs one declared query (dsh `query`): the platform fork with
    /// the provider lookup first, exactly the dsh control flow — the
    /// empty table makes both arms refuse through the not-registered
    /// texts.
    ///
    /// # Errors
    ///
    /// The dsh not-registered refusal for the requested platform.
    pub fn query(
        &self,
        platform: &str,
        provider: &str,
        _method: &str,
        _input: Option<&Value>,
        _agent: &str,
    ) -> Result<Value, BodyError> {
        let missing = match platform {
            "host" => format!("Host Cordis inspect provider \"{provider}\" is not registered"),
            _ => format!("Client Cordis inspect provider \"{provider}\" is not registered"),
        };
        Err(BodyError::plain(missing))
    }
}

/// Parses the `cordis_inspect_list` arguments (the dsh `{}` parameter
/// schema: the body stand-in for the schema validation, so a
/// non-object argument set refuses before execution).
///
/// # Errors
///
/// The non-object refusal.
pub fn parse_list_args(arguments: &Value) -> Result<(), BodyError> {
    if !arguments.is_object() {
        return Err(BodyError::plain("arguments must be an object"));
    }
    Ok(())
}

/// Parses the `cordis_inspect_query` arguments (the dsh parameter
/// schema's four rows: the platform enum, the two required names, and
/// the optional json input — the body stand-in for the schema
/// validation).
///
/// # Errors
///
/// The four shape refusals.
pub fn parse_query_args(
    arguments: &Value,
) -> Result<(String, String, String, Option<Value>), BodyError> {
    if !arguments.is_object() {
        return Err(BodyError::plain("arguments must be an object"));
    }
    let platform = arguments
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| BodyError::plain("platform must be a string in {host, client}"))?;
    if platform != "host" && platform != "client" {
        return Err(BodyError::plain(
            "platform must be a string in {host, client}",
        ));
    }
    let provider = arguments
        .get("provider")
        .and_then(Value::as_str)
        .ok_or_else(|| BodyError::plain("provider must be a string"))?;
    let method = arguments
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| BodyError::plain("method must be a string"))?;
    Ok((
        platform.to_owned(),
        provider.to_owned(),
        method.to_owned(),
        arguments.get("input").cloned(),
    ))
}

/// Executes the `cordis_inspect_list` call (the dsh `execute`): the
/// provider directory over the honest empty registry.
///
/// # Errors
///
/// The argument refusal.
pub fn execute_list(registry: &InspectRegistry, arguments: &Value) -> Result<Value, BodyError> {
    parse_list_args(arguments)?;
    Ok(json!({ "providers": registry.list() }))
}

/// Executes the `cordis_inspect_query` call (the dsh `execute`): the
/// agent gate and the platform fork over the honest empty registry.
///
/// # Errors
///
/// The argument refusals, the missing-agent refusal, or the dsh
/// not-registered refusal.
pub fn execute_query(
    registry: &InspectRegistry,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let (platform, provider, method, input) = parse_query_args(arguments)?;
    let Some(agent) = agent else {
        return Err(BodyError::plain(
            "Cordis inspection requires an Agent-backed session",
        ));
    };
    let data = registry.query(&platform, &provider, &method, input.as_ref(), agent)?;
    Ok(json!({
        "platform": platform,
        "provider": provider,
        "method": method,
        "data": data,
    }))
}

/// Renders one output value (the dsh `output.render` pair: one text
/// block carrying the pretty JSON).
#[must_use]
pub fn render(_args: &Value, value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    serde_json::json!([{ "type": "text", "text": text }])
}

/// The `cordis_inspect_list` pending-call presentation (dsh
/// `presentInspectListCall`).
#[must_use]
pub fn present_call_list() -> Value {
    json!({
        "card": "generic",
        "kind": "read",
        "title": "List Cordis Inspect Providers",
    })
}

/// The `cordis_inspect_query` pending-call presentation (dsh
/// `presentInspectQueryCall`).
#[must_use]
pub fn present_call_query(arguments: &Value) -> Value {
    let platform = arguments
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let provider = arguments
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let method = arguments
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "card": "generic",
        "kind": "read",
        "title": format!("Query Cordis {platform} {provider}.{method}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The empty registry reports no providers and both query arms
    /// refuse through the dsh not-registered texts.
    #[test]
    fn registry_stays_empty_and_refuses_honestly() {
        let registry = InspectRegistry::new();
        assert_eq!(registry.list(), Vec::<Value>::new());
        assert_eq!(
            registry
                .query("host", "acme.widgets", "schema", None, "agent-1")
                .unwrap_err()
                .message,
            "Host Cordis inspect provider \"acme.widgets\" is not registered"
        );
        assert_eq!(
            registry
                .query("client", "acme.widgets", "schema", None, "agent-1")
                .unwrap_err()
                .message,
            "Client Cordis inspect provider \"acme.widgets\" is not registered"
        );
    }

    /// The list execution returns the honest empty directory without
    /// touching the arguments beyond the shape gate.
    #[test]
    fn execute_list_returns_the_empty_directory() {
        let registry = InspectRegistry::new();
        let value = execute_list(&registry, &json!({})).expect("list");
        assert_eq!(value, serde_json::json!({ "providers": [] }));
        let refused = execute_list(&registry, &json!([1])).unwrap_err();
        assert_eq!(refused.message, "arguments must be an object");
    }

    /// The query execution wraps the refusal through the result
    /// envelope (the dsh execute shape: platform/provider/method echo
    /// on success only).
    #[test]
    fn execute_query_refuses_when_the_provider_is_unknown() {
        let registry = InspectRegistry::new();
        let arguments = json!({
            "platform": "host",
            "provider": "tools",
            "method": "schema",
        });
        let error = execute_query(&registry, &arguments, Some("agent-1")).unwrap_err();
        assert_eq!(
            error.message,
            "Host Cordis inspect provider \"tools\" is not registered"
        );
        // The missing-agent refusal.
        let error = execute_query(&registry, &arguments, None).unwrap_err();
        assert_eq!(
            error.message,
            "Cordis inspection requires an Agent-backed session"
        );
    }

    /// The query parse legs pin the four schema rows' runtime
    /// stand-ins (the enum gate, the two required names, and the
    /// optional input pass-through).
    #[test]
    fn parse_query_args_pins_the_rows() {
        let arguments = json!({
            "platform": "client",
            "provider": "p",
            "method": "m",
            "input": { "x": 1 },
        });
        let (platform, provider, method, input) = parse_query_args(&arguments).expect("query args");
        assert_eq!(platform, "client");
        assert_eq!(provider, "p");
        assert_eq!(method, "m");
        assert_eq!(input, Some(serde_json::json!({ "x": 1 })));
        for broken in [
            serde_json::json!({ "platform": 42, "provider": "p", "method": "m" }),
            serde_json::json!({ "platform": "edge", "provider": "p", "method": "m" }),
            serde_json::json!({ "platform": "host", "method": "m" }),
            serde_json::json!({ "platform": "host", "provider": "p" }),
            serde_json::json!([1]),
        ] {
            assert!(
                parse_query_args(&broken).is_err(),
                "refused: {broken}"
            );
            assert!(
                execute_query(&InspectRegistry::new(), &broken, Some("a")).is_err(),
                "refused: {broken}"
            );
        }
    }
}
