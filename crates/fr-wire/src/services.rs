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
    /// `{id}` → `{"header": EpochHeader | null}` — the log's latest
    /// `request/header` snapshot (the dsh `session.requestHeader()`; the
    /// Q16b-1A route-gate seam).
    pub const METHOD_REQUEST_HEADER: u32 = 10;
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
    /// Register one tool whose body rides wire endpoints (16a: the
    /// registration carries inline schema fields and
    /// `{service, method}` endpoints for execute/render/present).
    pub const METHOD_REGISTER: u32 = 13;
    /// Detach one wire-registered tool by id.
    pub const METHOD_UNREGISTER: u32 = 14;
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

/// The `'subprocess'` service face (owner crate `fr-shell`).
///
/// 16a band judgment: three services ride one export table, so every
/// method number stays unique across the whole artifact (the
/// `session-projections`/`sandbox` 1000+ precedent) — `subprocess`
/// keeps the low band, `jobs` and `shell` occupy the 1000+ / 2000+
/// bands.
pub mod subprocess {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "subprocess";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 0;
    /// Spawn one fully-specified subprocess (collect/pipe/inherit
    /// per-stream stdio) and return its handle id.
    pub const METHOD_SPAWN: u32 = 1;
    /// Read one collected stream from a byte offset without consuming.
    pub const METHOD_READ: u32 = 2;
    /// Settle one handle's exit facts (managed-range wait, bounded).
    pub const METHOD_WAIT: u32 = 3;
    /// Begin the managed-range termination procedure (idempotent).
    pub const METHOD_TERMINATE: u32 = 4;
    /// Resolve a bare PATH name or absolute path to an executable.
    pub const METHOD_RESOLVE_EXECUTABLE: u32 = 5;
}

/// The `'jobs'` service face (owner crate `fr-shell`).
pub mod jobs {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "jobs";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 1000;
    /// List caller-owned and unowned jobs in registration order.
    pub const METHOD_LIST: u32 = 1001;
    /// Project one job without changing its cursor.
    pub const METHOD_GET: u32 = 1002;
    /// Consume the ring from the model cursor (the `result` rides the
    /// first post-settlement read).
    pub const METHOD_READ: u32 = 1003;
    /// Read retained output at an absolute offset without moving the
    /// model cursor.
    pub const METHOD_READ_AT: u32 = 1004;
    /// Request cancellation, then mark the job stopping.
    pub const METHOD_KILL: u32 = 1005;
    /// Wait for settlement or the bounded timeout (slice waits only —
    /// the instance lock spans each call).
    pub const METHOD_WAIT: u32 = 1006;
    /// Drop one settled job's record from the visible set.
    pub const METHOD_REMOVE: u32 = 1007;
    /// Kill every job registered under one tool call (the caller-abort
    /// kill carrier; idempotent against settled or absent jobs).
    pub const METHOD_KILL_BY_TOOL_CALL: u32 = 1008;
}

/// The `'shell'` service face (owner crate `fr-shell`).
pub mod shell {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "shell";
    /// Shell metadata echo.
    pub const METHOD_DESCRIBE: u32 = 2000;
    /// The mounted executor's default sandbox mode (`{}` →
    /// `{mode: word | null}`) — the capability fact dsh exposes as
    /// `ctx.shell.sandboxMode`.
    pub const METHOD_SANDBOX_MODE: u32 = 2001;
    /// Apply implementation defaults and caps to one request.
    pub const METHOD_RESOLVE: u32 = 2002;
    /// Register the command as a job and spawn it (the dsh bash tool's
    /// `startJob`, absorbed into the seam because the output-source
    /// closures cannot cross the wire).
    pub const METHOD_START: u32 = 2003;
    /// The foreground projection of one absorbed job's process (the dsh
    /// tool's `process.result()`; the batch-2 promote path's
    /// settled-while-waiting arm consumes it).
    pub const METHOD_RESULT: u32 = 2004;
}

/// The `'fs'` service face (Q93-A + Q16b-3B rulings).
///
/// Owner crate `fr-tools-builtin`. The method band is this artifact's
/// 1000+ range (the fr-shell jobs precedent: the per-artifact method
/// space is unique across the whole export table, so the domain band
/// starts past the tool segments at 20-82). The vocabulary mirrors dsh
/// `@deepseek-ai/dsh-fs` verbatim: opaque `targetKey`/`version`
/// strings, the 13-code `FsErrorCode` taxonomy, and the atomic
/// write/edit outcomes.
pub mod fs {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "fs";
    /// Face metadata echo (`{plugin, service, methods}`).
    pub const METHOD_DESCRIBE: u32 = 1000;
    /// Resolve a model/plugin path to `{target: {target_key, display_path}}`
    /// (request `{path, cwd?}`; the missing-suffix realpath walk of dsh
    /// `resolveLocalTarget`).
    pub const METHOD_RESOLVE: u32 = 1001;
    /// `{target}` → `{path}` — the canonical absolute path a subprocess
    /// in this execution world can open (the opaque key spelled out).
    pub const METHOD_PROCESS_PATH: u32 = 1002;
    /// `{hostPath}` → `{path: string | null}` — the host-path mapping
    /// (the local backend resolves absolute spellings, else `null`).
    pub const METHOD_PROCESS_PATH_FROM_HOST_PATH: u32 = 1003;
    /// `{target}` → `{url}` — the canonical `file:` URI.
    pub const METHOD_FILE_URL: u32 = 1004;
    /// `{parent, child}` → `{contained: bool}` — canonical containment
    /// without exposing target keys.
    pub const METHOD_CONTAINS: u32 = 1005;
    /// `{target}` → `{info: {version, kind, size?} | null}`.
    pub const METHOD_STAT: u32 = 1006;
    /// `{path, cwd?}` → `{info: {version, kind, size?} | null}` without
    /// following the final symlink component.
    pub const METHOD_LSTAT: u32 = 1007;
    /// `{target}` → `{text}` — the whole decoded UTF-8 text (binary and
    /// invalid UTF-8 reject with the typed codes).
    pub const METHOD_READ_TEXT: u32 = 1008;
    /// `{target}` → `{chunks: [...]}` — the streamed whole-file text
    /// as decoded chunks in one bounded reply (the dsh async iterable
    /// materialized; in-crate consumers use the pull iterator instead).
    pub const METHOD_STREAM_TEXT: u32 = 1009;
    /// `{target, max_bytes}` → `{bytes: base64}` — raw bytes with the
    /// inclusive cap (`FS_TOO_LARGE` past it).
    pub const METHOD_READ_BYTES: u32 = 1010;
    /// `{target, offset, length}` → `{bytes: base64}` — one raw window.
    pub const METHOD_READ_BYTE_RANGE: u32 = 1011;
    /// `{target}` → `{entries: [...]}` — direct children in stable name
    /// order.
    pub const METHOD_LIST_DIR: u32 = 1012;
    /// `{target, content, expected?, policy?}` → the write
    /// outcome (`{operation, version, before, after}`).
    pub const METHOD_WRITE_TEXT: u32 = 1013;
    /// `{target, edit, expected?, policy?}` → the edit outcome
    /// (`{version, before, after}`).
    pub const METHOD_EDIT_TEXT: u32 = 1014;
    /// `{target}` → the dsh base-contract refusal (`Filesystem watching
    /// is not supported by this provider.`) — the fs-local watcher
    /// override lands with its first consumer (the Q105′A sequencing).
    pub const METHOD_WATCH: u32 = 1015;
}

/// The `'spill-store'` service face (the Q16b-2A seam ruling).
///
/// The owner crate is `fr-compaction` at Phase 17 — the face exists now
/// so the search tools ship their final-form consumption. Vocabulary
/// mirrors dsh `@deepseek-ai/dsh-spill`'s `SaveTextSpill`/`SpillRef`.
pub mod spill_store {
    /// The provided service name consumers require.
    pub const SERVICE_NAME: &str = "spill-store";
    /// Persist one complete formatted result (`SaveTextSpill`) → the
    /// reference (`{locator, retrievalHint, ...}`).
    pub const METHOD_SAVE_TEXT: u32 = 0;
}

/// The `'attachments'` service face (the Q16b-1A durable-store seam).
///
/// Vocabulary mirrors dsh `@deepseek-ai/dsh-attachment`'s store face:
/// the limits snapshot the tool bodies read, and the image save the
/// `read_image` tool calls. Byte payloads ride `crate::base64`.
pub mod attachments {
    /// The provided service name consumers inject.
    pub const SERVICE_NAME: &str = "attachments";
    /// Face metadata echo (`{plugin, service, abiVersion}`).
    pub const METHOD_DESCRIBE: u32 = 1;
    /// `{data: base64, mediaType, name?}` → the durable reference
    /// (`ImageAttachmentRef` wire form; the dsh `saveImage`).
    pub const METHOD_SAVE_IMAGE: u32 = 2;
    /// `{}` → the admission limits snapshot (the dsh `imageLimits`).
    pub const METHOD_IMAGE_LIMITS: u32 = 3;
}

/// The `'llm'` service face (the fr-llm shell's provided service).
///
/// The generation trio rides the runtime generation hub; the
/// resolve-model-info arm (the Q16b-1A route gate's seam) resolves one
/// route's exact model metadata without starting a generation.
pub mod llm {
    /// The provided service name consumers inject (PRD :336 `inject llm`).
    pub const SERVICE_NAME: &str = "llm";
    /// Face metadata echo (`{name, service, abiVersion}`).
    pub const METHOD_DESCRIBE: u32 = 1;
    /// `{request: CallConfig}` → the prepared-call binding (the
    /// runtime `prepareCall`).
    pub const METHOD_PREPARE_CALL: u32 = 2;
    /// `{request: GenerateOptions, preparedId?}` → `{generationId}`.
    pub const METHOD_BEGIN: u32 = 3;
    /// `{generationId}` → the buffered batches.
    pub const METHOD_DRAIN: u32 = 4;
    /// `{generationId}` → `{}`.
    pub const METHOD_ABORT: u32 = 5;
    /// `{provider, model}` → `WireModelInfo` — exact-route model metadata
    /// without starting a generation (the dsh `llm.resolveModelInfo`).
    pub const METHOD_RESOLVE_MODEL_INFO: u32 = 6;
}

// Rust guideline compliant 2026-10-29
