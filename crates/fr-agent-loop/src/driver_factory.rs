// Rust guideline compliant 2026-11-04
//! The production driver assembly (the dsh `agents.create`/`resume`
//! factory legs over the wire face, agent-loop/index.ts:750-935).
//!
//! The lifecycle arms in [`crate::plugin_shell`] parse the wire payload,
//! drive the persistence face, and then land here: one production
//! [`DriverDeps`] over the activation's outbound seam, one restored
//! [`LoopDriver`], and one spawned driver thread — the thread the
//! `teardown` join half waits on.

use crate::driver::{DriverDeps, LoopDriver};
use crate::llm_client::LlmClient;
use crate::outbound::Outbound;
use crate::prompt_client::{InstructionsClient, SystemPromptClient};
use crate::session_reads::SessionReadsClient;
use crate::tools_client::ToolsClient;
use fr_wire::ids::MessageId;

/// The parallel-tool-call cap when no configuration channel narrows it
/// (the dsh loop config default, agent-loop/src/constants.ts:6).
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 10;

/// Validates the wire `agentOptions.maxTokens` cap (the dsh
/// `assertAgentOptions`, agent-loop/index.ts:202-207; the refusal text
/// is verbatim).
///
/// # Errors
///
/// Refuses zero and any value past the safe-integer ceiling.
pub fn validated_max_tokens(max_tokens: Option<u64>) -> Result<Option<u64>, String> {
    match max_tokens {
        None => Ok(None),
        Some(value) if value > 0 && value <= fr_wire::ids::SAFE_INTEGER_MAX => Ok(Some(value)),
        Some(_) => Err("agent maxTokens must be a positive safe integer".to_owned()),
    }
}

/// Assembles one session's production [`DriverDeps`] over the
/// activation's outbound seam.
///
/// The consumed services are the artifact's declared injects:
/// `'session-persistence'` (reads), `'system-prompt'` (prompt plus the
/// workspace-instructions compose leg — the dsh mounts
/// `agent-instructions` unconditionally), `'llm'`, and `'tools'`. The
/// `'skills'` pre-step bridge stays unwired: the service is not in this
/// artifact's inject list (the dsh loop inject list omits skills the
/// same way — the transform is a composition-level mount). The parallel
/// cap is the baked dsh default (the dylib descriptor is static, so no
/// runtime configuration channel exists).
#[must_use]
pub fn production_deps(outbound: Outbound, session_id: &str, cwd: &str) -> DriverDeps {
    DriverDeps {
        reads: Box::new(SessionReadsClient::new(outbound, session_id)),
        prompt: Box::new(SystemPromptClient::new(outbound)),
        llm: Box::new(LlmClient::new(outbound)),
        tools: Box::new(ToolsClient::new(outbound, session_id)),
        instructions: Some(Box::new(InstructionsClient::new(outbound, session_id, cwd))),
        // No `'skills'` inject: the transform is a composition-level
        // mount (the dsh loop inject list omits skills identically).
        skills: None,
        // The wall clock for stream chunk timestamps (the fr-session
        // shell's millisecond judgment).
        clock: Box::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
                })
        }),
        // Stable identity for created messages (the dsh `randomUUID`).
        next_id: Box::new(|| MessageId::new(uuid::Uuid::new_v4().to_string())),
        max_parallel_tool_calls: Box::new(|| DEFAULT_MAX_PARALLEL_TOOL_CALLS),
        // The Q102′A interjection mirror: fire-and-forget, keyed by
        // `agent` (the guard consumers' reset-chain key). An emit
        // refusal is contained — the notification face never blocks
        // (decision 46) and a missed reset only risks an early
        // reminder, never a lost message.
        emit_user_admitted: Box::new(move |agent: &str| {
            let payload = serde_json::json!({ "agent": agent });
            if let Ok(body) = serde_json::to_vec(&payload) {
                let _ = outbound.emit(fr_wire::services::events::EVENT_USER_ADMITTED, &body);
            }
        }),
    }
}

/// Spawns the session's driver thread running [`LoopDriver::run`].
///
/// The handle lands in the shell's driver registry so the `teardown`
/// broadcast-and-join half observes it (the v3 thread discipline: every
/// plugin-owned thread joins before `teardown` returns).
///
/// # Errors
///
/// Returns the spawn refusal (thread name or resource exhaustion).
pub fn spawn_driver(
    session_id: &str,
    driver: LoopDriver,
) -> Result<std::thread::JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name(format!("fr-agent-loop:{session_id}"))
        .spawn(move || {
            let mut driver = driver;
            driver.run();
        })
        .map_err(|error| format!("driver thread spawn failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_PARALLEL_TOOL_CALLS, validated_max_tokens};

    #[test]
    fn the_parallel_cap_carries_the_dsh_default() {
        assert_eq!(DEFAULT_MAX_PARALLEL_TOOL_CALLS, 10);
    }

    #[test]
    fn the_max_tokens_cap_follows_the_dsh_refusals() {
        assert_eq!(validated_max_tokens(None), Ok(None));
        assert_eq!(validated_max_tokens(Some(1)), Ok(Some(1)));
        assert_eq!(
            validated_max_tokens(Some(fr_wire::ids::SAFE_INTEGER_MAX)),
            Ok(Some(fr_wire::ids::SAFE_INTEGER_MAX))
        );
        let error = validated_max_tokens(Some(0)).expect_err("zero refuses");
        assert_eq!(error, "agent maxTokens must be a positive safe integer");
        let error = validated_max_tokens(Some(fr_wire::ids::SAFE_INTEGER_MAX + 1))
            .expect_err("unsafe integer refuses");
        assert_eq!(error, "agent maxTokens must be a positive safe integer");
    }
}
