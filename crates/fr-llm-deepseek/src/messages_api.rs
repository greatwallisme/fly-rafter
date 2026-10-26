//! Shared Messages API endpoint and header policy (dsh `messages-api.ts`),
//! plus the request-header assembly of dsh `adapter.ts` `request`.
//!
//! PRD scenario (fr provider rule): fly-rafter sends no dsh-branded
//! harness headers — the `x-deepseek-harness-*` trio of dsh is fr's own
//! recorded deviation and stays absent here.

use fr_llm::attribution::attribution_headers;

use crate::auth::auth_headers;
use crate::wire_types::WireBlock;

/// Required opt-in for Messages file operations and file-referenced image
/// requests.
pub const MESSAGES_FILES_BETA: &str = "files-api-2025-04-14";

/// Required opt-in for mid-conversation `tool_addition` and `tool_removal`
/// blocks.
pub const MESSAGES_TOOL_CHANGES_BETA: &str = "mid-conversation-tool-changes-2026-07-01";

/// The wire protocol version header value (dsh `adapter.ts:144`).
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Resolve the API root without duplicating an explicit provider version
/// path (dsh `messagesApiRoot`): the root beneath which Messages resources
/// are exposed.
#[must_use]
pub fn messages_api_root(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let ends_with_v1 = match base.split_once("://") {
        Some((_scheme, rest)) => match rest.split_once('/') {
            Some((_authority, path)) => {
                let path = format!("/{path}");
                path.ends_with("/v1")
            }
            None => false,
        },
        None => false,
    };
    if ends_with_v1 {
        base.to_owned()
    } else {
        format!("{base}/v1")
    }
}

/// The `anthropic-beta` opt-ins one request needs (dsh `adapter.ts`
/// `betas`).
///
/// The Files beta rides when file ids do; the tool-changes beta rides when
/// any conversation block activates or removes a declared tool.
#[must_use]
pub fn request_betas(
    has_file_ids: bool,
    messages: &[crate::wire_types::WireMessage],
) -> Vec<&'static str> {
    let mut betas = Vec::new();
    if has_file_ids {
        betas.push(MESSAGES_FILES_BETA);
    }
    if messages.iter().any(|message| {
        message.content.iter().any(|block| {
            matches!(
                block,
                WireBlock::ToolAddition { .. } | WireBlock::ToolRemoval { .. }
            )
        })
    }) {
        betas.push(MESSAGES_TOOL_CHANGES_BETA);
    }
    betas
}

/// The request headers in dsh insertion order, minus the harness-branded
/// trio (PRD provider rule): attribution, content negotiation, the
/// credential header, the protocol version, and the beta opt-ins.
#[must_use]
pub fn request_headers(api_key: &str, betas: &[&str]) -> Vec<(&'static str, String)> {
    let mut headers: Vec<(&'static str, String)> = attribution_headers().into_iter().collect();
    headers.push(("content-type", "application/json".to_owned()));
    headers.push(("accept", "text/event-stream".to_owned()));
    headers.extend(auth_headers(api_key));
    headers.push(("anthropic-version", ANTHROPIC_VERSION.to_owned()));
    if !betas.is_empty() {
        headers.push(("anthropic-beta", betas.join(",")));
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_types::{ToolReference, ToolReferenceKind, WireMessage, WireRole};

    fn message(blocks: Vec<WireBlock>) -> WireMessage {
        WireMessage {
            role: WireRole::User,
            content: blocks,
        }
    }

    #[test]
    fn root_completion_pins_v1() {
        assert_eq!(
            messages_api_root("https://api.example.com"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            messages_api_root("https://api.example.com/"),
            "https://api.example.com/v1"
        );
        // An explicit /v1 stays unduplicated, at any depth.
        assert_eq!(
            messages_api_root("https://api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(
            messages_api_root("https://api.example.com/gateway/v1/"),
            "https://api.example.com/gateway/v1"
        );
        assert_eq!(
            messages_api_root("https://api.example.com/anthropic"),
            "https://api.example.com/anthropic/v1"
        );
    }

    /// The deepseek-wire scenario: the request targets base-url with the
    /// protocol version header and carries none of the dsh-branded
    /// harness extensions (`dsh_session_log`, `dsh_plugin_packages`,
    /// `x-deepseek-harness-*`).
    #[test]
    fn request_headers_pin_dsh_order_without_harness() {
        let headers = request_headers("sk-abc123", &[]);
        let names: Vec<&str> = headers.iter().map(|(name, _value)| *name).collect();
        assert_eq!(
            names,
            [
                "user-agent",
                "content-type",
                "accept",
                "x-api-key",
                "anthropic-version",
            ]
        );
        let values: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        assert!(values.contains(&("x-api-key", "sk-abc123")));
        assert!(values.contains(&("anthropic-version", "2023-06-01")));
        assert!(values.contains(&("content-type", "application/json")));
        assert!(values.contains(&("accept", "text/event-stream")));
        let user_agent = headers
            .iter()
            .find(|(name, _value)| *name == "user-agent")
            .map(|(_name, value)| value.clone())
            .unwrap_or_default();
        assert_eq!(user_agent, fr_llm::attribution::user_agent());
        for (name, value) in &headers {
            assert!(
                !name.starts_with("x-deepseek-harness"),
                "harness header leaked: {name}={value}"
            );
            assert!(*name != "dsh_session_log" && *name != "dsh_plugin_packages");
        }
    }

    #[test]
    fn betas_follow_file_ids_and_tool_changes() {
        let tool_block = || WireBlock::ToolAddition {
            tool: ToolReference {
                kind: ToolReferenceKind::ToolReference,
                name: "sh".to_owned(),
            },
        };
        assert_eq!(
            request_betas(false, &[message(vec![tool_block()])]),
            [MESSAGES_TOOL_CHANGES_BETA]
        );
        assert_eq!(
            request_betas(true, &[message(Vec::new())]),
            [MESSAGES_FILES_BETA]
        );
        assert_eq!(
            request_betas(false, &[message(Vec::new())]),
            [] as [&str; 0]
        );
        assert_eq!(
            request_betas(true, &[message(vec![tool_block()])]),
            [MESSAGES_FILES_BETA, MESSAGES_TOOL_CHANGES_BETA]
        );
    }

    /// The `v1_sse` scenario: the resolved path forces the /v1 completion and
    /// a recorded frame whose `event:` name disagrees with `event.type` is
    /// rejected as `MALFORMED_RESPONSE`.
    #[test]
    fn v1_path_completion_and_sse_mismatch() {
        assert_eq!(
            messages_api_root("https://gateway.internal"),
            "https://gateway.internal/v1"
        );
        let error = crate::sse::parse_sse(
            "event: content_block_start\ndata: {\"type\": \"message_start\"}\n\n",
            &mut || {},
        )
        .expect_err("frame mismatch");
        assert_eq!(error.code(), crate::codes::MALFORMED_RESPONSE_CODE);
        assert_eq!(error.message(), "DeepSeek Messages SSE event type mismatch");
    }
}

// Rust guideline compliant 2026-10-26
