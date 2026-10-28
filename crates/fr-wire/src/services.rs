//! Cross-plugin service-face contracts.
//!
//! Every dougong service name and method selector plugins call across
//! the dynamic channel (PRD 4.4: the shared wire shapes live in this
//! pure serde library, each artifact linking its own copy).
//!
//! Batch 2a placement judgment: plugin crates' shells export one
//! `DOUGONG_PLUGIN_EXPORTS` symbol each, and a test binary linking two
//! plugin rlibs collides on it at link time — so consumers share the
//! face constants through this shell-free library instead of importing
//! them from the owning crates' modules. Every owning crate pins its
//! local selectors against this table in its own tests (the safe
//! direction: one plugin crate plus this library, never two plugin
//! crates in one link).

/// The `'agent'` service face (owner crate `fr-agent-loop`).
pub mod agent {
    /// The provided service name consumers call.
    pub const SERVICE_NAME: &str = "agent";
    /// Queue one input with full target and wake control (dsh `send`).
    pub const METHOD_SEND: u32 = 0;
    /// Queue one steering input (dsh `steer`).
    pub const METHOD_STEER: u32 = 1;
    /// Queue one non-waking injection (dsh `inject`).
    pub const METHOD_INJECT: u32 = 2;
    /// Queue one follow-up input (dsh `followup`).
    pub const METHOD_FOLLOWUP: u32 = 3;
    /// Request cancellation (dsh `cancel`).
    pub const METHOD_CANCEL: u32 = 4;
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 5;
    /// Query the session's loop status.
    pub const METHOD_STATUS: u32 = 6;
}

/// The `'session-persistence'` service face (owner crate `fr-session`).
pub mod session_persistence {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "session-persistence";
    /// Declare durable event type names at activation time.
    pub const METHOD_DECLARE_EVENT_TYPES: u32 = 0;
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 1;
    /// Create a session (prepare + ownership + publish).
    pub const METHOD_SESSION_CREATE: u32 = 2;
    /// Append one typed event.
    pub const METHOD_SESSION_APPEND: u32 = 3;
    /// Run the flush checkpoint for one session.
    pub const METHOD_SESSION_FLUSH: u32 = 4;
    /// Fork one live session.
    pub const METHOD_SESSION_FORK: u32 = 5;
    /// Resume a stored session.
    pub const METHOD_SESSION_RESUME: u32 = 6;
    /// Rebuild a session in memory from a seed payload.
    pub const METHOD_SESSION_RESTORE: u32 = 7;
    /// Drain the outbound mirror lane.
    pub const METHOD_SESSION_DRAIN: u32 = 8;
    /// Read one live session's log slice (Q107-A).
    pub const METHOD_SESSION_READ: u32 = 9;
}

/// The `'system-prompt'` service face (owner crate `fr-system-prompt`).
pub mod system_prompt {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "system-prompt";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 1;
    /// Assemble the system prompt for one context.
    pub const METHOD_ASSEMBLE: u32 = 2;
    /// Refresh the context providers of one assembly.
    pub const METHOD_REFRESH_CONTEXT: u32 = 3;
    /// Register one static section.
    pub const METHOD_REGISTER_SECTION: u32 = 4;
    /// Register one runtime-context entry.
    pub const METHOD_REGISTER_CONTEXT: u32 = 5;
    /// Register one prompt variable.
    pub const METHOD_REGISTER_VARIABLE: u32 = 6;
    /// Register one tool-schema provider.
    pub const METHOD_REGISTER_TOOLS: u32 = 7;
    /// Unregister one named contribution.
    pub const METHOD_UNREGISTER: u32 = 8;
    /// Compose the instructions payload for one context.
    pub const METHOD_COMPOSE_INSTRUCTIONS: u32 = 9;
}

/// The `'tools'` service face (owner crate `fr-tools`).
pub mod tools {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "tools";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 0;
    /// Register one policy endpoint on a decision stage (Q101′A).
    pub const METHOD_REGISTER_LISTENER: u32 = 1;
    /// Detach one registered endpoint.
    pub const METHOD_UNREGISTER_LISTENER: u32 = 2;
    /// Classify one call's execution mode.
    pub const METHOD_EXECUTION_MODE: u32 = 3;
    /// Stage 1: mint the execution and run the gate.
    pub const METHOD_PREPARE: u32 = 4;
    /// Stage 2: spawn the dispatch worker.
    pub const METHOD_DISPATCH: u32 = 5;
    /// Pull the settled outcomes.
    pub const METHOD_SETTLED: u32 = 6;
    /// Stage 4 for a settled result.
    pub const METHOD_FINALIZE: u32 = 7;
    /// Stage 4 for a final-result settlement.
    pub const METHOD_FINISH: u32 = 8;
    /// The composite pipeline on the calling thread.
    pub const METHOD_EXECUTE: u32 = 9;
    /// The model-facing schema projection for the current mode.
    pub const METHOD_WIRE_SCHEMAS: u32 = 10;
    /// Abort the caller signal of one in-flight call.
    pub const METHOD_CANCEL: u32 = 11;
    /// Install or clear the wire approval endpoint (Q109-A).
    pub const METHOD_SET_APPROVAL: u32 = 12;
}

/// The `'approval'` service face (owner crate `fr-approval`).
pub mod approval {
    /// The provided service name consumers call.
    pub const SERVICE_NAME: &str = "approval";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 0;
    /// One turn-enclosed approval ask.
    pub const METHOD_REQUEST: u32 = 1;
    /// The live policy switch with its user notice.
    pub const METHOD_SET_POLICY: u32 = 2;
    /// The session override fold without defaults.
    pub const METHOD_OVERRIDE_OF: u32 = 3;
    /// Append one answerer endpoint to the chain (Q108-A).
    pub const METHOD_REGISTER_ANSWERER: u32 = 4;
    /// Remove one answerer by token.
    pub const METHOD_UNREGISTER_ANSWERER: u32 = 5;
    /// The `approval:policy` runtime-context endpoint.
    pub const METHOD_CONTEXT_TEXT: u32 = 6;
}

/// The `'session-projections'` service face (owner crate `fr-session`).
///
/// Q105′A/Q110′A′: the seam's two M1 faces ride the same artifact that
/// provides `session-persistence`, and the dougong dynamic channel hands a
/// plugin only the method selector — so this service's selectors occupy the
/// 1000+ band to stay unique across both faces of one export table.
pub mod session_projections {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "session-projections";
    /// Register one projection unit (`{key, stateVersion, fold}`).
    ///
    /// Q110′A′: `fold` names the registrant's pure folding endpoint
    /// (full log to state, no outbound requires inside).
    pub const METHOD_REGISTER: u32 = 1000;
    /// Read one unit's state (`{id, key}`) — the registry relay.
    pub const METHOD_STATE_OF: u32 = 1001;
}

/// The `'permission-presets'` service face (owner crate
/// `fr-permission-presets`).
pub mod permission_presets {
    /// The provided service name consumers call.
    pub const SERVICE_NAME: &str = "permission-presets";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 0;
    /// The process-level preset catalog (dsh `catalog` remote).
    pub const METHOD_CATALOG: u32 = 1;
    /// The `permissions` projection's pure folding endpoint — the full
    /// log (`{header, inheritedEventCount, events}`) to `{state}` (Q110′A′).
    pub const METHOD_FOLD: u32 = 2;
    /// The `/permission` command's behavior face (`{agent, rawInput}`) —
    /// the leg the Phase 18 command registry calls.
    pub const METHOD_COMMAND: u32 = 3;
}

/// The `'sandbox-policy'` service face (owner crate `fr-sandbox`).
///
/// Batch 2b owner correction: batch 2a shipped a consumed subset with a
/// speculative `0 = declare-event-types` reservation, but the owner face
/// follows the approval precedent (a policy seam declares its own events
/// through `'session-persistence'`, it does not accept declarations), so
/// `describe` owns selector 0.
pub mod sandbox_policy {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "sandbox-policy";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 0;
    /// Read the deployment's default sandbox mode (`{}` → `{mode}`) — the
    /// capability fact dsh exposes as `ctx.shell.sandboxMode`.
    pub const METHOD_SANDBOX_MODE: u32 = 1;
    /// Resolve one capability call's complete policy
    /// (`{session?, mode?}` → `{mode, workspace_root, session_id?}`).
    pub const METHOD_RESOLVE: u32 = 2;
    /// The `sandbox:policy` runtime-context endpoint (`{agent?}` →
    /// `{text}`; the approval `context-text` precedent).
    pub const METHOD_CONTEXT_TEXT: u32 = 3;
    /// The `sandboxMode` projection's pure folding endpoint — the full log
    /// (`{header, inheritedEventCount, events}`) to `{state}` (Q110′A′).
    pub const METHOD_FOLD: u32 = 4;
}

/// The `'sandbox'` process-confinement seam face (owner crate `fr-sandbox`).
///
/// Batch 2b band judgment: this service rides the same export table as
/// `sandbox-policy`, so its selectors occupy the 1000+ band (the
/// `session-projections` precedent — the dynamic channel hands a plugin
/// only the method selector).
pub mod sandbox {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "sandbox";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 1000;
    /// Wrap one exact argv confined under a per-call policy
    /// (`{argv, policy}` → the `ConfinedArgv` wire shape) or fail closed
    /// with `sandbox-unavailable`.
    pub const METHOD_CONFINE: u32 = 1001;
}

// Rust guideline compliant 2026-10-29
