// Rust guideline compliant 2026-10-29
//! The shell-tool bodies (dsh `tool-bash` + `tool-pwsh` `execute`,
//! `presentCall`, and `presentResult`, call-for-call twins).
//!
//! The bodies consume the executor, the registry, and the sibling
//! domains through one dependency-inverted seam ([`Seams`]): the wire
//! service implements it over the host vtable, and the tests script
//! it. The M1 composition always backs the tools with the job registry
//! (the job-backed registration is the only variant), so a foreground
//! call is a job the body waits on through the bounded wait face, and
//! the caller-abort kill reaches the job through the registry's
//! kill-by-tool-call face rather than through this plugin (whose
//! instance lock the blocked execute call holds).

use serde_json::{Value, json};

use crate::render;
use crate::shell_env::{Execution, FrozenEnv, ShellEnvRegistry};

/// The dsh `TOOL_ABORTED` code (dsh `tools` package constant).
pub const TOOL_ABORTED: &str = "TOOL_ABORTED";

/// The escalation words the twin bodies advertise and start with (the
/// dsh `ESCALATION_TARGETS` order).
const SHELL_ESCALATION_MODES: [&str; 2] = ["workspace-write", "danger-full-access"];

/// The bounded wait slice (milliseconds). The fr-shell instance lock
/// spans each wait call, so the body re-locks in slices of this size;
/// the fr-tools abort watcher's queued kill-by-tool-call lands in a
/// between-slice gap (the fr-wire jobs `METHOD_WAIT` contract).
const WAIT_SLICE_MS: f64 = 200.0;

/// The expiry grace (milliseconds): how long the wait keeps slicing
/// after the budget runs out so a kill requested just before exhaustion
/// lands before the promoted/preparation decision reads the job.
const WAIT_GRACE_MS: f64 = 300.0;

/// The body failure over the wire (dsh `HarnessError` projection).
#[derive(Debug, Clone)]
pub struct BodyError {
    /// The harness error code, when the failure is routable (abort).
    pub code: Option<String>,
    /// The remote error class name.
    pub name: String,
    /// The failure message.
    pub message: String,
}

impl BodyError {
    /// A plain (non-routable) failure (dsh plain `Error`).
    #[must_use]
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            code: None,
            name: "Error".to_owned(),
            message: message.into(),
        }
    }

    /// The structured caller-abort failure (dsh `toolAborted`).
    #[must_use]
    pub fn aborted() -> Self {
        Self {
            code: Some(TOOL_ABORTED.to_owned()),
            name: "AbortError".to_owned(),
            message: "tool call aborted".to_owned(),
        }
    }
}

/// The executor seam (dsh `ctx.shell.sandboxMode` reduced to the
/// policy-resolution face the bodies need; the mode read itself rides
/// the service's registration-time read).
pub trait PolicySeam: Send + Sync {
    /// Resolves the complete standing policy for one call.
    ///
    /// # Errors
    ///
    /// The policy service failure.
    fn resolve_policy(&self, session: Option<&str>) -> Result<Option<Value>, String>;
}

/// The approval seam (dsh `ctx.get('approval')` reduced to the ask
/// face [`fr_sandbox::escalation::EscalationApprover`] consumes).
pub trait ApprovalSeam: Send + Sync {
    /// Asks the human; the outcome word is the closed vocabulary.
    ///
    /// # Errors
    ///
    /// A transport failure (mapped to `unavailable` by the caller —
    /// the ask never throws).
    fn request(&self, ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String>;
}

/// The composed cross-plugin seam the shell-tool bodies run through.
pub trait Seams: Send + Sync {
    /// The sandbox policy seam.
    fn policy(&self) -> &dyn PolicySeam;
    /// The approval seam.
    fn approval(&self) -> &dyn ApprovalSeam;
    /// Resolves one execution request (dsh `ctx.shell.resolve`).
    ///
    /// # Errors
    ///
    /// The executor's resolve refusal family.
    fn shell_resolve(&self, shell: &str, request: &Value) -> Result<Value, String>;
    /// Registers and starts one job (dsh `startJob`); the reply is the
    /// job id and the resolved workdir.
    ///
    /// # Errors
    ///
    /// The registry's admission refusal family.
    #[expect(
        clippy::too_many_arguments,
        reason = "the seam mirrors the dsh startJob closure shape: shell, spec, command, owner, call identity, and the escalation table"
    )]
    fn shell_start(
        &self,
        shell: &str,
        spec: &Value,
        command: &str,
        owner: Option<&str>,
        call_id: &str,
        root_call_id: &str,
        escalation_modes: &[&str],
    ) -> Result<(String, String), String>;
    /// The absorbed process's foreground projection (dsh
    /// `process.result()`; fail-fast on an unsettled or never-started
    /// process).
    ///
    /// # Errors
    ///
    /// The projection's own failure family (the error texts classify
    /// the wait arms).
    fn shell_result(&self, job_id: &str) -> Result<Value, String>;
    /// Waits on one job until settlement or the timeout (dsh
    /// `registry.wait`; expiry returns the running view).
    ///
    /// # Errors
    ///
    /// An unknown or foreign job, or an invalid timeout.
    fn jobs_wait(&self, id: &str, timeout_ms: f64, caller: Option<&str>) -> Result<Value, String>;
    /// One consuming read (dsh `registry.read`).
    ///
    /// # Errors
    ///
    /// An unknown or foreign job.
    fn jobs_read(&self, id: &str, caller: Option<&str>) -> Result<Value, String>;
    /// Kills one job (dsh `registry.kill`); the reply word rides
    /// through.
    ///
    /// # Errors
    ///
    /// An unknown or foreign job, or a failing cancel.
    fn jobs_kill(
        &self,
        id: &str,
        caller: Option<&str>,
        reason: Option<&str>,
    ) -> Result<String, String>;
    /// Removes one settled record (dsh `registry.remove`).
    ///
    /// # Errors
    ///
    /// An unknown or foreign job, or a still-running one.
    fn jobs_remove(&self, id: &str, caller: Option<&str>) -> Result<(), String>;
    /// The working-directory `ensure` (dsh
    /// `ctx.workingDirectory.ensure`).
    ///
    /// # Errors
    ///
    /// The minimal-arm failure family.
    fn working_directory_ensure(&self, agent: &str) -> Result<String, String>;
    /// The managed-environment snapshot (dsh `ctx.shellEnv.collect`).
    ///
    /// # Errors
    ///
    /// The collection refusal family.
    fn shell_env_collect(&self, agent: Option<&str>) -> Result<FrozenEnv, String>;
    /// Spawns one fully-specified subprocess and returns its handle id
    /// (dsh `ctx.subprocess.spawn`; the search tools' foreground runs).
    ///
    /// # Errors
    ///
    /// The spawn refusal family.
    fn subprocess_spawn(&self, spec: &Value) -> Result<String, String>;
    /// Reads one collected stream from a byte offset (dsh
    /// `handle.collected.*.readFrom`).
    ///
    /// # Errors
    ///
    /// The read refusal family.
    fn subprocess_read(&self, handle: &str, stream: &str, from: u64) -> Result<Value, String>;
    /// Settles one handle's exit facts under a bounded wait (dsh
    /// `handle.done`, sliced).
    ///
    /// # Errors
    ///
    /// The wait refusal family.
    fn subprocess_wait(&self, handle: &str, timeout_ms: f64) -> Result<Value, String>;
    /// Resolves a bare PATH name or absolute path to an executable (dsh
    /// has no analog: the packaged binary; the fr `FR_RIPGREP`/PATH
    /// resolution rides this face).
    ///
    /// # Errors
    ///
    /// The resolution refusal family.
    fn subprocess_resolve_executable(&self, command: &str) -> Result<String, String>;
    /// Persists one complete formatted result (dsh
    /// `ctx.spillStore.saveText`); `Ok(None)` is the absent backend (the
    /// dsh `ctx.get` miss), not a storage failure.
    ///
    /// # Errors
    ///
    /// The storage failure family (the dsh `saveText` rejection).
    fn spill_store_save_text(&self, save: &Value) -> Result<Option<Value>, String>;
}

/// The shell variant the bodies parameterize over (dsh's two tool
/// packages as one twin skeleton).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellVariant {
    /// The bash tool (`bash`).
    Bash,
    /// The pwsh tool (`pwsh`).
    Pwsh,
}

impl ShellVariant {
    /// The registered tool name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Pwsh => "pwsh",
        }
    }

    /// The job kind the absorbed start registers.
    #[must_use]
    pub const fn shell_word(self) -> &'static str {
        self.name()
    }

    /// The model-facing description (the dsh twin descriptions).
    #[must_use]
    pub fn description(self, escalation: bool) -> String {
        match self {
            Self::Bash => crate::schema::bash_description(),
            Self::Pwsh => crate::schema::pwsh_description(escalation),
        }
    }

    /// The `[stderr]`-section heading twin (the section title is
    /// shared; the variant exists for the pwsh table's example text).
    #[must_use]
    pub const fn command_word(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Pwsh => "PowerShell",
        }
    }
}

/// The parsed model arguments (dsh `BashToolArgs`/`PwshToolArgs`).
#[derive(Debug, Clone, Default)]
pub struct ShellArgs {
    /// The command source.
    pub command: String,
    /// The UI description.
    pub description: String,
    /// Timeout override.
    pub timeout_ms: Option<f64>,
    /// Working-directory override.
    pub workdir: Option<String>,
    /// Background request.
    pub run_in_background: bool,
    /// Escalation target.
    pub sandbox_permissions: Option<String>,
    /// Escalation reason.
    pub justification: Option<String>,
}

/// Parses the wire arguments object (absent keys stay absent; the
/// schema's declared types are re-checked at the value level).
#[must_use]
pub fn parse_args(arguments: &Value) -> ShellArgs {
    let word = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    ShellArgs {
        command: word("command").unwrap_or_default(),
        description: word("description").unwrap_or_default(),
        timeout_ms: arguments.get("timeoutMs").and_then(Value::as_f64),
        workdir: word("workdir"),
        run_in_background: arguments.get("run_in_background").and_then(Value::as_bool)
            == Some(true),
        sandbox_permissions: word("sandbox_permissions"),
        justification: word("justification"),
    }
}

/// Validates the value constraints the parameter table cannot express
/// (dsh `validateBashArgs`).
///
/// The bash arm skips the escalation pairing when the requested mode
/// equals the effective one; the pwsh arm always pairs.
///
/// # Errors
///
/// The dsh refusal texts verbatim.
pub fn validate_args(
    variant: ShellVariant,
    args: &ShellArgs,
    effective_mode: Option<&str>,
) -> Result<(), String> {
    if args.command.trim().is_empty() {
        return Err("invalid command: expected a non-empty string".to_owned());
    }
    if args.description.trim().is_empty() {
        return Err("invalid description: expected a non-empty string".to_owned());
    }
    if let Some(timeout) = args.timeout_ms
        && (!timeout.is_finite() || timeout <= 0.0)
    {
        return Err(format!(
            "invalid timeoutMs: expected a positive number, got {}",
            render::js_number_value(&json!(timeout))
        ));
    }
    if variant == ShellVariant::Bash
        && args.sandbox_permissions.is_some()
        && args.sandbox_permissions.as_deref() == effective_mode
    {
        return Ok(());
    }
    // The bash twin normalizes an empty justification away when no
    // escalation is requested (so a bare empty string passes); the pwsh
    // twin hands the raw pairing over and refuses the orphan.
    let justification = if variant == ShellVariant::Bash {
        match args.justification.as_deref() {
            Some(text) if text.trim().is_empty() && args.sandbox_permissions.is_none() => None,
            other => other,
        }
    } else {
        args.justification.as_deref()
    };
    fr_sandbox::escalation::validate_escalation_args(
        args.sandbox_permissions.as_deref(),
        justification,
    )
}

/// The timeoutMs parameter's two description variants (dsh bakes the
/// promote fact at registration): the promote arm is the catalog-frozen
/// constant, the kill arm stays the composed source text.
///
/// The catalog documents the promote registration alone.
#[must_use]
pub fn timeout_ms_description(promote: bool) -> String {
    if promote {
        crate::catalog_gen::BASH_TIMEOUT_MS_PROMOTE_TEXT.to_owned()
    } else {
        "Timeout in milliseconds. The executor applies its configured default and cap, and kills \
the command on expiry."
            .to_owned()
    }
}

/// Resolves a relative per-call override against the session's current
/// directory (dsh `resolveWorkdir`; the bash twin string-joins, the
/// pwsh twin resolves `..` segments).
#[must_use]
pub fn resolve_workdir(
    variant: ShellVariant,
    model_workdir: Option<&str>,
    session_cwd: Option<&str>,
) -> Option<String> {
    let Some(model) = model_workdir else {
        return session_cwd.map(str::to_owned);
    };
    match session_cwd {
        Some(session) if !std::path::Path::new(model).is_absolute() => {
            Some(match variant {
                ShellVariant::Bash => format!("{session}/{model}"),
                // The pwsh twin normalizes dot segments (dsh
                // `path.resolve`).
                ShellVariant::Pwsh => normalize_join(session, model),
            })
        }
        _ => Some(model.to_owned()),
    }
}

/// Lexically joins and collapses `.`/`..` segments (the dsh pwsh twin's
/// `path.resolve(session, model)` shape).
#[must_use]
fn normalize_join(session: &str, model: &str) -> String {
    let joined_path = format!("{session}/{model}");
    let mut segments: Vec<&str> = Vec::new();
    for part in joined_path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let joined = segments.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        format!("/{joined}")
    }
}

/// The dsh `JSON.stringify` quoting.
fn quote(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// Builds the executor request (dsh `ShellExecRequest` wire form: the
/// managed snapshot merges after the ordinary environment, the sandbox
/// policy rides when resolved).
#[must_use]
pub fn build_request(
    args: &ShellArgs,
    workdir: Option<&str>,
    env: &FrozenEnv,
    policy: Option<&Value>,
) -> Value {
    let mut request = serde_json::Map::new();
    request.insert("command".to_owned(), Value::String(args.command.clone()));
    if let Some(dir) = workdir {
        request.insert("workdir".to_owned(), Value::String(dir.to_owned()));
    }
    if let Some(timeout) = args.timeout_ms {
        request.insert("timeout_ms".to_owned(), json!(timeout));
    }
    request.insert("fr_env".to_owned(), env.to_json());
    if let Some(policy) = policy {
        request.insert("sandbox_policy".to_owned(), policy.clone());
    }
    Value::Object(request)
}

/// The approver bridge: the approval seam folded into the escalation
/// facet [`fr_sandbox::escalation::EscalationApprover`] consumes (every
/// transport failure resolves `unavailable` — the ask never throws).
struct SeamApprover<'a>(&'a dyn ApprovalSeam);

impl fr_sandbox::escalation::EscalationApprover for SeamApprover<'_> {
    fn request(
        &self,
        ask: &fr_sandbox::escalation::EscalationAsk,
    ) -> fr_sandbox::escalation::EscalationOutcome {
        let Ok(word) = self.0.request(ask) else {
            return fr_sandbox::escalation::EscalationOutcome::Unavailable;
        };
        fr_sandbox::escalation::EscalationOutcome::parse(&word)
            .unwrap_or(fr_sandbox::escalation::EscalationOutcome::Unavailable)
    }
}

/// The parsed canonical foreground value the body returns (the dsh
/// canonical projection plus the variant's `kind`; the bash twin
/// prepends the kind, the pwsh twin closes with it — the wire key order
/// each tool's wire form carries).
fn canonical_foreground(variant: ShellVariant, result: &Value, cwd: &str) -> Value {
    let mut value = serde_json::Map::new();
    let insert_kind = |map: &mut serde_json::Map<String, Value>| {
        map.insert("kind".to_owned(), Value::String("foreground".to_owned()));
    };
    if variant == ShellVariant::Bash {
        insert_kind(&mut value);
    }
    value.insert("cwd".to_owned(), Value::String(cwd.to_owned()));
    for key in [
        "exitCode",
        "signal",
        "timedOut",
        "aborted",
        "timeoutMs",
        "stdout",
        "stderr",
        "sandbox",
    ] {
        if let Some(field) = result.get(key) {
            value.insert(key.to_owned(), field.clone());
        }
    }
    if variant == ShellVariant::Pwsh {
        insert_kind(&mut value);
    }
    Value::Object(value)
}

/// Extracts the external kill reason the dsh `stopped` closure carried
/// (the fr-shell settlement merges it behind the base detail; see the
/// batch-2 design pins).
#[must_use]
pub fn stopped_reason(detail: Option<&str>) -> Option<String> {
    let detail = detail?;
    let (base, rest) = detail.split_once("; ")?;
    if base.starts_with("signal: ") || base == "killed before exit" {
        return Some(rest.to_owned());
    }
    None
}

/// The escalation arm (dsh `approveBashEscalation` twin): resolves a
/// `sandbox_permissions` request through the approval seam BEFORE
/// anything executes.
///
/// Unadvertised fields still reach execute (the schema checks
/// advertised keys only); the shared fail-closed sequence lives in
/// fr-sandbox.
fn resolve_escalation(
    variant: ShellVariant,
    seams: &dyn Seams,
    args: &ShellArgs,
    standing_policy: Option<&Value>,
    call_id: &str,
    agent: Option<&str>,
) -> Result<Option<fr_sandbox::seam::SandboxMode>, BodyError> {
    if !(args.sandbox_permissions.is_some() && args.justification.is_some()) {
        return Ok(None);
    }
    let standing = standing_policy.ok_or_else(|| {
        BodyError::plain(
            "sandbox_permissions is not available in this composition (no sandboxing \
executor to escalate)",
        )
    })?;
    let effective = fr_sandbox::seam::SandboxMode::parse(
        standing
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .ok_or_else(|| BodyError::plain("the standing policy mode is not a sandbox mode"))?;
    let request = fr_sandbox::escalation::EscalationRequest {
        requested_mode: args.sandbox_permissions.clone().expect("checked above"),
        justification: args.justification.clone().expect("checked above"),
        effective_mode: effective,
        subject: "command",
    };
    let agent_word = agent.ok_or_else(|| {
        BodyError::plain(
            "sandbox escalation requires approval, but the call has no agent to route it \
through",
        )
    })?;
    let approval = fr_sandbox::escalation::EscalationApproval {
        approver: Some(&SeamApprover(seams.approval())),
        agent: Some(agent_word),
        call_id,
        tool_name: variant.name(),
    };
    let granted = fr_sandbox::escalation::approve_escalation(&request, &approval)
        .map_err(BodyError::plain)?;
    Ok(Some(granted))
}

/// The background arm (dsh `run_in_background`): resolve without a
/// deadline, start detached, and return the handle at once.
fn start_background(
    variant: ShellVariant,
    seams: &dyn Seams,
    args: &ShellArgs,
    request: &Value,
    owner: Option<&str>,
    call_id: &str,
    root_call_id: &str,
) -> Result<Value, BodyError> {
    let mut background_request = request.clone();
    if let Some(object) = background_request.as_object_mut() {
        object.insert("on_expiry".to_owned(), Value::String("none".to_owned()));
    }
    let reply = seams
        .shell_resolve(variant.shell_word(), &background_request)
        .map_err(BodyError::plain)?;
    let spec = reply.get("spec").cloned().unwrap_or(Value::Null);
    let workdir = spec
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let (job_id, _) = seams
        .shell_start(
            variant.shell_word(),
            &spec,
            &args.command,
            owner,
            call_id,
            root_call_id,
            &SHELL_ESCALATION_MODES,
        )
        .map_err(BodyError::plain)?;
    Ok(json!({ "kind": "background", "jobId": job_id, "cwd": workdir }))
}

/// Executes one shell call (the dsh `execute` twin; the job-backed
/// path is the only path — the M1 composition always has `jobs`).
///
/// # Errors
///
/// The validation, escalation, working-directory, and registry refusal
/// families, and the structured caller-abort failure.
pub fn execute(
    variant: ShellVariant,
    seams: &dyn Seams,
    env_registry: &ShellEnvRegistry,
    arguments: &Value,
    call_id: &str,
    root_call_id: &str,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let args = parse_args(arguments);
    let escalation_modes = SHELL_ESCALATION_MODES;
    // The standing policy resolves first for bash (the validation arm
    // reads its mode); pwsh validates before touching the service.
    let standing_policy = if variant == ShellVariant::Bash {
        seams
            .policy()
            .resolve_policy(agent)
            .map_err(BodyError::plain)?
    } else {
        None
    };
    let effective_mode = standing_policy
        .as_ref()
        .and_then(|policy| policy.get("mode"))
        .and_then(Value::as_str);
    validate_args(variant, &args, effective_mode).map_err(BodyError::plain)?;
    let standing_policy = if variant == ShellVariant::Pwsh {
        seams
            .policy()
            .resolve_policy(agent)
            .map_err(BodyError::plain)?
    } else {
        standing_policy
    };
    let approved_mode = resolve_escalation(
        variant,
        seams,
        &args,
        standing_policy.as_ref(),
        call_id,
        agent,
    )?;
    let policy = match (&standing_policy, approved_mode) {
        (Some(standing), Some(mode)) => {
            let mut merged = standing.clone();
            if let Some(object) = merged.as_object_mut() {
                object.insert("mode".to_owned(), Value::String(mode.as_str().to_owned()));
            }
            Some(merged)
        }
        (Some(standing), None) => Some(standing.clone()),
        (None, _) => None,
    };
    let session_cwd = match agent {
        Some(agent) => Some(
            seams
                .working_directory_ensure(agent)
                .map_err(BodyError::plain)?,
        ),
        None => None,
    };
    let workdir = resolve_workdir(variant, args.workdir.as_deref(), session_cwd.as_deref());
    let env = env_registry
        .collect(&Execution {
            agent: agent.map(str::to_owned),
        })
        .map_err(BodyError::plain)?;
    let request = build_request(&args, workdir.as_deref(), &env, policy.as_ref());
    let owner = agent;
    if args.run_in_background {
        return start_background(
            variant,
            seams,
            &args,
            &request,
            owner,
            call_id,
            root_call_id,
        );
    }
    // The foreground call is a job the body waits on (dsh promote path;
    // the admission-refused fallback runs under the deadline kill — the
    // start refusal surfaces as the error result).
    let (job_id, spec_workdir, spec_timeout) = start_foreground(
        variant,
        seams,
        &args,
        &request,
        owner,
        call_id,
        root_call_id,
    )?;
    wait_on_job(
        seams,
        variant,
        &job_id,
        owner,
        spec_timeout,
        &spec_workdir,
        policy.as_ref(),
        &escalation_modes,
    )
}

/// The foreground arm's resolve and start (dsh promote path): the
/// resolved spec loses its deadline (the job carries the wait), and the
/// registry admission refusal surfaces as the error result.
fn start_foreground(
    variant: ShellVariant,
    seams: &dyn Seams,
    args: &ShellArgs,
    request: &Value,
    owner: Option<&str>,
    call_id: &str,
    root_call_id: &str,
) -> Result<(String, String, f64), BodyError> {
    let mut foreground_request = request.clone();
    if let Some(object) = foreground_request.as_object_mut() {
        object.insert("on_expiry".to_owned(), Value::String("none".to_owned()));
    }
    let reply = seams
        .shell_resolve(variant.shell_word(), &foreground_request)
        .map_err(BodyError::plain)?;
    let spec = reply.get("spec").cloned().unwrap_or(Value::Null);
    let spec_workdir = spec
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let spec_timeout = spec
        .get("timeout_ms")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let (job_id, _) = seams
        .shell_start(
            variant.shell_word(),
            &spec,
            &args.command,
            owner,
            call_id,
            root_call_id,
            &SHELL_ESCALATION_MODES,
        )
        .map_err(BodyError::plain)?;
    Ok((job_id, spec_workdir, spec_timeout))
}

/// Waits on the registered foreground command until it settles or the
/// timeout promotes it (dsh `waitOnJob`).
///
/// # Errors
///
/// The preparation-timeout arm: nothing is running to keep, so the stop
/// runs on this call's account and the settled empty result is what the
/// executor's own deadline would have given (dsh
/// `timed out during preparation`).
fn preparation_timeout_value(
    seams: &dyn Seams,
    job_id: &str,
    owner: Option<&str>,
    timeout_ms: f64,
    spec_workdir: &str,
    policy: Option<&Value>,
) -> Result<Value, BodyError> {
    seams
        .jobs_kill(job_id, owner, Some("timed out during preparation"))
        .map_err(BodyError::plain)?;
    let _ = seams.jobs_wait(job_id, timeout_ms.max(5_000.0), owner);
    let _ = seams.jobs_remove(job_id, owner);
    let mut empty = serde_json::Map::new();
    empty.insert("kind".to_owned(), Value::String("foreground".to_owned()));
    empty.insert("cwd".to_owned(), Value::String(spec_workdir.to_owned()));
    empty.insert("exitCode".to_owned(), Value::Null);
    empty.insert("signal".to_owned(), Value::Null);
    empty.insert("timedOut".to_owned(), Value::Bool(true));
    empty.insert("aborted".to_owned(), Value::Bool(false));
    empty.insert("timeoutMs".to_owned(), json!(timeout_ms));
    empty.insert(
        "stdout".to_owned(),
        json!({ "text": "", "truncated": false }),
    );
    empty.insert(
        "stderr".to_owned(),
        json!({ "text": "", "truncated": false }),
    );
    if let Some(mode) = policy
        .and_then(|policy| policy.get("mode"))
        .and_then(Value::as_str)
    {
        empty.insert(
            "sandbox".to_owned(),
            json!({ "mode": mode, "denied": false }),
        );
    }
    Ok(Value::Object(empty))
}

/// The arms' refusal families and the structured caller-abort failure.
/// One bounded wait slice (the fr-shell instance lock spans each call,
/// so the body re-locks in bounded slices).
fn slice_wait(
    seams: &dyn Seams,
    job_id: &str,
    owner: Option<&str>,
    slice_ms: f64,
) -> Result<Value, BodyError> {
    seams
        .jobs_wait(job_id, slice_ms, owner)
        .map_err(BodyError::plain)
}

/// Whether the view still reports an unsettled job.
fn still_running(view: &Value) -> bool {
    matches!(
        view.get("status").and_then(Value::as_str),
        Some("running" | "stopping")
    )
}

/// The sliced wait (the fr-wire jobs `METHOD_WAIT` contract: slice waits
/// only, because the fr-shell instance lock spans each call): bounded
/// logical slices keep the lock free between calls, which is what lets
/// the fr-tools abort watcher's queued kill-by-tool-call land while this
/// call still waits — dsh rejects the wait itself on abort, and here the
/// kill surfaces as the settled killed view the settled arm maps. The
/// expiry grace then absorbs a kill still in flight at budget exhaustion
/// so a dying call never promotes (the residual race — an abort inside
/// the grace window itself — is carrier plumbing bounded by the watcher
/// poll interval).
#[expect(
    clippy::too_many_arguments,
    reason = "the arms mirror the dsh waitOnJob closure captures: the job identity, the wait budget, the resolved spec facts, and the advertised escalation table"
)]
fn wait_on_job(
    seams: &dyn Seams,
    variant: ShellVariant,
    job_id: &str,
    owner: Option<&str>,
    timeout_ms: f64,
    spec_workdir: &str,
    policy: Option<&Value>,
    escalation_modes: &[&str],
) -> Result<Value, BodyError> {
    let wait_slice = timeout_ms.min(WAIT_SLICE_MS);
    let mut view = slice_wait(seams, job_id, owner, wait_slice)?;
    let mut consumed = wait_slice;
    while still_running(&view) {
        let budget_left = timeout_ms - consumed;
        if budget_left <= 0.0 {
            break;
        }
        let slice = budget_left.min(WAIT_SLICE_MS);
        view = slice_wait(seams, job_id, owner, slice)?;
        consumed += slice;
    }
    if still_running(&view) {
        // The expiry grace: a kill requested just before budget
        // exhaustion is still queued on the instance lock; the grace
        // slices free the lock until it lands (the next slice then reads
        // the settled killed view) or the grace itself runs out.
        let mut grace_left = WAIT_GRACE_MS;
        loop {
            if grace_left <= 0.0 {
                break;
            }
            let slice = grace_left.min(WAIT_SLICE_MS);
            view = slice_wait(seams, job_id, owner, slice)?;
            if !still_running(&view) {
                break;
            }
            grace_left -= slice;
        }
    }
    if still_running(&view) {
        // The wait expired. Nothing running to keep when the spawn never
        // published: the deadline's preparation timeout, with the empty
        // settled result the executor's own deadline would have given.
        let published = match seams.shell_result(job_id) {
            Ok(_) => true,
            Err(error) => {
                if error.contains("process did not start") {
                    false
                } else if error.contains("has not settled") {
                    true
                } else {
                    return Err(BodyError::plain(error));
                }
            }
        };
        if !published {
            let value =
                preparation_timeout_value(seams, job_id, owner, timeout_ms, spec_workdir, policy)?;
            return Ok(value);
        }
        // The job keeps running: one consuming read seeds the promoted
        // hand-off, so job_output continues exactly where this result
        // stops (the live promotion carries no sandbox notes — the dsh
        // proc.sandbox exists only after settlement).
        let read = seams.jobs_read(job_id, owner).map_err(BodyError::plain)?;
        let delta = ring_delta(read.get("chunks"));
        let lossy = read.get("lossy").and_then(Value::as_bool) == Some(true);
        let spill_paths: Vec<String> = read
            .pointer("/job/output/spillPaths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let output = render::render_job_read(&delta, lossy, &spill_paths, None, escalation_modes);
        return Ok(json!({
            "kind": "promoted",
            "cwd": spec_workdir,
            "jobId": job_id,
            "timeoutMs": timeout_ms,
            "output": output,
        }));
    }
    // Settled while the call waited: the model never saw the id, so the
    // record leaves the registry with this result.
    let detail = view
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_owned);
    // The caller-abort kill (the reason the abort propagation face
    // kills with) maps to the structured abort, never a result value.
    if detail.as_deref() == Some("tool call aborted")
        || detail
            .as_deref()
            .is_some_and(|text| text.ends_with("; tool call aborted"))
    {
        let _ = seams.jobs_remove(job_id, owner);
        return Err(BodyError::aborted());
    }
    let result = match seams.shell_result(job_id) {
        Ok(result) => result,
        Err(error) => {
            // No process was published: the starter failed before the
            // spawn, and the job's terminal detail is the failure the
            // foreground projection would have rejected with.
            let _ = seams.jobs_remove(job_id, owner);
            return Err(BodyError::plain(detail.unwrap_or(error)));
        }
    };
    let _ = seams.jobs_remove(job_id, owner);
    let mut value = canonical_foreground(variant, &result, spec_workdir);
    if let Some(reason) = stopped_reason(detail.as_deref()) {
        value
            .as_object_mut()
            .expect("a freshly built object")
            .insert("stopped".to_owned(), Value::String(reason));
    }
    Ok(value)
}

/// Merges the read chunks into the shell tools' rendered delta (dsh
/// `ringDelta`): stdout chunks in order, then every stderr chunk in one
/// `[stderr]` section.
#[must_use]
pub fn ring_delta(chunks: Option<&Value>) -> String {
    let Some(chunks) = chunks.and_then(Value::as_array) else {
        return String::new();
    };
    let mut out = String::new();
    let mut err = String::new();
    for chunk in chunks {
        let text = chunk
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if chunk.get("channel").and_then(Value::as_str) == Some("stderr") {
            err.push_str(text);
        } else {
            out.push_str(text);
        }
    }
    if err.is_empty() {
        return out;
    }
    let separator = if !out.is_empty() && !out.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{out}{separator}[stderr]\n{err}")
}

/// Presents the pending call (dsh `presentBashCall` twin): a background
/// request is a generic execute card; a foreground call is a terminal
/// card carrying the command, the description, and the per-call cwd.
#[must_use]
pub fn present_call(arguments: &Value) -> Value {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let description = arguments
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if arguments.get("run_in_background").and_then(Value::as_bool) == Some(true) {
        return json!({
            "card": "generic",
            "title": command,
            "kind": "execute",
            "rawInput": command,
            "content": [{ "type": "text", "text": description }],
        });
    }
    let mut card = serde_json::Map::new();
    card.insert("card".to_owned(), Value::String("terminal".to_owned()));
    card.insert("title".to_owned(), Value::String(command.to_owned()));
    card.insert(
        "description".to_owned(),
        Value::String(description.to_owned()),
    );
    if let Some(cwd) = arguments.get("workdir") {
        card.insert("cwd".to_owned(), cwd.clone());
    }
    Value::Object(card)
}

/// Presents the completed result (dsh `presentBashResult` twin).
///
/// Background acknowledgements, promotions, and errors render as
/// generic fenced output without an exit pill; a settled foreground run
/// renders as a terminal card whose exit marker becomes the pill.
#[must_use]
pub fn present_result(arguments: &Value, result: &Value) -> Option<Value> {
    let content = result.get("content")?.as_array()?;
    if content.len() != 1 {
        return None;
    }
    let block = content.first()?;
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let raw = block.get("text").and_then(Value::as_str)?;
    let is_background = arguments.get("run_in_background").and_then(Value::as_bool) == Some(true);
    let is_promoted = result
        .get("value")
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        == Some("promoted");
    let is_error = result.get("isError").and_then(Value::as_bool) == Some(true);
    if is_background || is_promoted || is_error {
        let trimmed = raw.trim_end_matches('\n');
        return Some(json!({
            "card": "generic",
            "content": [{ "type": "text", "text": format!("```console\n{trimmed}\n```") }],
        }));
    }
    // The exit marker becomes the card's exit pill, so it leaves the
    // output body.
    match render::parse_exit_status(raw) {
        render::ParsedExitStatus::Signal { body, signal } => Some(json!({
            "card": "terminal",
            "output": body,
            "signal": signal,
        })),
        render::ParsedExitStatus::ExitCode { body, code } => {
            if code == 0 {
                Some(json!({ "card": "terminal", "output": body }))
            } else {
                Some(json!({ "card": "terminal", "output": body, "exitCode": code }))
            }
        }
    }
}

/// Quotes a path the dsh refusal way (the `JSON.stringify` form).
#[must_use]
pub fn quote_value(text: &str) -> String {
    quote(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The scripted job state one fake registry serves.
    #[derive(Debug, Default, Clone)]
    struct ScriptJob {
        wait_status: &'static str,
        wait_detail: Option<String>,
        read_chunks: Value,
        read_lossy: bool,
        spill_paths: Vec<String>,
        result: Option<Value>,
        result_error: Option<String>,
    }

    /// The scripted seams: one policy value, one approval outcome word,
    /// one job script, and recorded calls.
    struct ScriptSeams {
        policy: Option<Value>,
        approval_word: String,
        job: Mutex<ScriptJob>,
        started: Mutex<Vec<(String, String)>>,
        kills: Mutex<Vec<(String, Option<String>)>>,
        removed: Mutex<Vec<String>>,
        existing_dir: &'static str,
        /// The wait-call count at which the scripted job flips to its
        /// killed-abort view (the watcher's kill landing between
        /// slices).
        flip_after: Option<usize>,
        wait_calls: std::sync::atomic::AtomicUsize,
    }

    impl std::fmt::Debug for ScriptSeams {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ScriptSeams").finish_non_exhaustive()
        }
    }

    impl ScriptSeams {
        fn settled(result: Value) -> Self {
            Self {
                policy: Some(json!({ "mode": "workspace-write", "workspace_root": "/ws" })),
                approval_word: "allowed-once".to_owned(),
                job: Mutex::new(ScriptJob {
                    wait_status: "completed",
                    wait_detail: Some("exit code: 0".to_owned()),
                    read_chunks: Value::Array(Vec::new()),
                    read_lossy: false,
                    spill_paths: Vec::new(),
                    result: Some(result),
                    result_error: None,
                }),
                started: Mutex::new(Vec::new()),
                kills: Mutex::new(Vec::new()),
                removed: Mutex::new(Vec::new()),
                existing_dir: "/ws",
                flip_after: None,
                wait_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn timed_out_running() -> Self {
            Self {
                policy: Some(json!({ "mode": "workspace-write", "workspace_root": "/ws" })),
                approval_word: "allowed-once".to_owned(),
                job: Mutex::new(ScriptJob {
                    wait_status: "running",
                    wait_detail: None,
                    read_chunks: json!([
                        { "at": 0, "text": "partial ", "channel": "stdout" },
                        { "at": 8, "text": "boom\n", "channel": "stderr" },
                    ]),
                    read_lossy: true,
                    spill_paths: vec!["/spill/out".to_owned()],
                    result: None,
                    result_error: Some("job bash-x has not settled".to_owned()),
                }),
                started: Mutex::new(Vec::new()),
                kills: Mutex::new(Vec::new()),
                removed: Mutex::new(Vec::new()),
                existing_dir: "/ws",
                flip_after: None,
                wait_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn preparation_timeout() -> Self {
            let mut seams = Self::timed_out_running();
            seams.job = Mutex::new(ScriptJob {
                wait_status: "running",
                wait_detail: None,
                read_chunks: Value::Array(Vec::new()),
                read_lossy: false,
                spill_paths: Vec::new(),
                result: None,
                result_error: Some("job bash-x process did not start".to_owned()),
            });
            seams
        }

        fn externally_killed() -> Self {
            Self {
                policy: Some(json!({ "mode": "workspace-write", "workspace_root": "/ws" })),
                approval_word: "allowed-once".to_owned(),
                job: Mutex::new(ScriptJob {
                    wait_status: "killed",
                    wait_detail: Some("signal: SIGTERM; the human stopped it".to_owned()),
                    read_chunks: Value::Array(Vec::new()),
                    read_lossy: false,
                    spill_paths: Vec::new(),
                    result: Some(json!({
                        "exitCode": null, "signal": "SIGTERM", "timedOut": false,
                        "aborted": true, "timeoutMs": 50.0,
                        "stdout": { "text": "", "truncated": false },
                        "stderr": { "text": "", "truncated": false },
                    })),
                    result_error: None,
                }),
                started: Mutex::new(Vec::new()),
                kills: Mutex::new(Vec::new()),
                removed: Mutex::new(Vec::new()),
                existing_dir: "/ws",
                flip_after: None,
                wait_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl crate::tools::PolicySeam for ScriptSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(self.policy.clone())
        }
    }

    impl ApprovalSeam for ScriptSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Ok(self.approval_word.clone())
        }
    }

    impl Seams for ScriptSeams {
        fn policy(&self) -> &dyn crate::tools::PolicySeam {
            self
        }

        fn approval(&self) -> &dyn ApprovalSeam {
            self
        }

        fn shell_resolve(&self, _shell: &str, request: &Value) -> Result<Value, String> {
            Ok(json!({
                "spec": {
                    "command": request["command"],
                    "workdir": self.existing_dir,
                    "timeout_ms": 50.0,
                    "on_expiry": "none",
                    "stdout_max_bytes": 64000.0,
                }
            }))
        }

        fn shell_start(
            &self,
            shell: &str,
            _spec: &Value,
            command: &str,
            _owner: Option<&str>,
            call_id: &str,
            _root_call_id: &str,
            _escalation_modes: &[&str],
        ) -> Result<(String, String), String> {
            self.started
                .lock()
                .expect("started")
                .push((format!("{shell}:{command}"), call_id.to_owned()));
            Ok(("bash-1".to_owned(), self.existing_dir.to_owned()))
        }

        fn shell_result(&self, _job_id: &str) -> Result<Value, String> {
            let job = self.job.lock().expect("job");
            if let Some(error) = &job.result_error {
                return Err(error.clone());
            }
            job.result
                .clone()
                .ok_or_else(|| "job bash-x has not settled".to_owned())
        }

        fn jobs_wait(
            &self,
            _id: &str,
            _timeout_ms: f64,
            _caller: Option<&str>,
        ) -> Result<Value, String> {
            let calls = self
                .wait_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            let mut job = self.job.lock().expect("job").clone();
            if job.wait_status == "running"
                && self.flip_after.is_some_and(|threshold| calls >= threshold)
            {
                // The watcher's kill landed between slices.
                job.wait_status = "killed";
                job.wait_detail = Some("signal: SIGTERM; tool call aborted".to_owned());
            }
            Ok(json!({
                "id": "bash-1",
                "status": job.wait_status,
                "detail": job.wait_detail,
                "output": { "total": 0, "earliest": 0 },
            }))
        }

        fn jobs_read(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            let job = self.job.lock().expect("job");
            Ok(json!({
                "chunks": job.read_chunks,
                "lossy": job.read_lossy,
                "job": {
                    "id": "bash-1",
                    "output": { "total": 0, "earliest": 0, "spillPaths": job.spill_paths },
                },
            }))
        }

        fn jobs_kill(
            &self,
            id: &str,
            _caller: Option<&str>,
            reason: Option<&str>,
        ) -> Result<String, String> {
            self.kills
                .lock()
                .expect("kills")
                .push((id.to_owned(), reason.map(str::to_owned)));
            Ok("requested".to_owned())
        }

        fn jobs_remove(&self, id: &str, _caller: Option<&str>) -> Result<(), String> {
            self.removed.lock().expect("removed").push(id.to_owned());
            Ok(())
        }

        fn working_directory_ensure(&self, _agent: &str) -> Result<String, String> {
            Ok(self.existing_dir.to_owned())
        }

        fn subprocess_spawn(&self, _spec: &Value) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn subprocess_read(
            &self,
            _handle: &str,
            _stream: &str,
            _from: u64,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_wait(&self, _handle: &str, _timeout_ms: f64) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_resolve_executable(&self, _command: &str) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn spill_store_save_text(&self, _save: &Value) -> Result<Option<Value>, String> {
            Err("unused".to_owned())
        }

        fn shell_env_collect(&self, agent: Option<&str>) -> Result<FrozenEnv, String> {
            let registry = ShellEnvRegistry::new(Some("/h")).expect("home");
            registry.collect(&Execution {
                agent: agent.map(str::to_owned),
            })
        }
    }

    fn arguments() -> Value {
        json!({
            "command": "echo hi",
            "description": "Print hi",
            "timeoutMs": 50,
        })
    }

    fn registry() -> ShellEnvRegistry {
        ShellEnvRegistry::new(Some("/h")).expect("home")
    }

    /// The settled foreground round trip: canonical foreground value
    /// with the bash kind first, one job registered and removed, and
    /// the cwd carried from the resolved spec.
    #[test]
    fn foreground_settles_canonically_and_removes_the_job() {
        let seams = ScriptSeams::settled(json!({
            "exitCode": 0, "signal": null, "timedOut": false, "aborted": false,
            "timeoutMs": 50.0,
            "stdout": { "text": "hi\n", "truncated": false },
            "stderr": { "text": "", "truncated": false },
        }));
        let value = execute(
            ShellVariant::Bash,
            &seams,
            &registry(),
            &arguments(),
            "c1",
            "c1",
            Some("s1"),
        )
        .expect("execute");
        assert_eq!(value["kind"], "foreground");
        assert_eq!(value["cwd"], "/ws");
        assert_eq!(value["exitCode"], 0);
        assert_eq!(
            seams.removed.lock().expect("removed").as_slice(),
            ["bash-1"]
        );
        assert_eq!(seams.started.lock().expect("started").len(), 1);
    }

    /// The timeout promotion hands off the consumed read: loss notice,
    /// spill path, the delta with the stderr section, and the job id.
    #[test]
    fn timeout_promotes_with_the_consumed_read() {
        let seams = ScriptSeams::timed_out_running();
        let value = execute(
            ShellVariant::Bash,
            &seams,
            &registry(),
            &arguments(),
            "c1",
            "c1",
            Some("s1"),
        )
        .expect("execute");
        assert_eq!(value["kind"], "promoted");
        assert_eq!(value["jobId"], "bash-1");
        assert_eq!(
            value["output"],
            "partial \n[stderr]\nboom\n[some output was dropped from memory; full output: /spill/out]"
        );
        assert!(seams.removed.lock().expect("removed").is_empty());
    }

    /// The preparation timeout stops the job and returns the settled
    /// empty shape.
    #[test]
    fn preparation_timeout_stops_and_returns_the_empty_shape() {
        let seams = ScriptSeams::preparation_timeout();
        let value = execute(
            ShellVariant::Bash,
            &seams,
            &registry(),
            &arguments(),
            "c1",
            "c1",
            Some("s1"),
        )
        .expect("execute");
        assert_eq!(value["timedOut"], true);
        assert_eq!(value["exitCode"], Value::Null);
        assert_eq!(value["stdout"]["text"], "");
        assert_eq!(
            seams.kills.lock().expect("kills").as_slice(),
            [(
                "bash-1".to_owned(),
                Some("timed out during preparation".to_owned())
            )]
        );
        assert_eq!(seams.removed.lock().expect("removed").len(), 1);
    }

    /// An external kill merges the recorded reason behind the canonical
    /// facts; a caller-abort kill maps to the structured abort.
    #[test]
    fn external_kill_merges_stopped_and_abort_maps_to_the_error() {
        let seams = ScriptSeams::externally_killed();
        let value = execute(
            ShellVariant::Bash,
            &seams,
            &registry(),
            &arguments(),
            "c1",
            "c1",
            Some("s1"),
        )
        .expect("execute");
        assert_eq!(value["stopped"], "the human stopped it");
        assert_eq!(value["signal"], "SIGTERM");

        let mut killed = ScriptSeams::externally_killed();
        let mut job = killed.job.lock().expect("job").clone();
        job.wait_detail = Some("signal: SIGTERM; tool call aborted".to_owned());
        killed.job = Mutex::new(job);
        let error = execute(
            ShellVariant::Bash,
            &killed,
            &registry(),
            &arguments(),
            "c1",
            "c1",
            Some("s1"),
        )
        .expect_err("aborted");
        assert_eq!(error.code.as_deref(), Some(TOOL_ABORTED));
        assert_eq!(error.name, "AbortError");
        assert_eq!(error.message, "tool call aborted");
    }

    /// The watcher's kill landing between slices surfaces as the
    /// structured abort: running slices until the scripted kill flips
    /// the job, then the settled killed view maps to the abort error and
    /// the record leaves the registry (the sliced-wait carrier chain).
    #[test]
    fn sliced_wait_maps_the_landing_abort_kill_to_the_abort_error() {
        let mut seams = ScriptSeams::timed_out_running();
        seams.flip_after = Some(3);
        let error = execute(
            ShellVariant::Bash,
            &seams,
            &registry(),
            &arguments(),
            "c1",
            "c1",
            Some("s1"),
        )
        .expect_err("aborted");
        assert_eq!(error.code.as_deref(), Some(TOOL_ABORTED));
        assert!(
            seams.wait_calls.load(std::sync::atomic::Ordering::SeqCst) > 1,
            "the wait sliced across several calls"
        );
        assert_eq!(
            seams.removed.lock().expect("removed").as_slice(),
            ["bash-1"],
            "the abort arm removes the record"
        );
    }

    /// The background arm resolves without a deadline and returns the
    /// handle at once; the escalation pair rides the approval seam and
    /// lands in the merged policy mode.
    #[test]
    fn background_returns_the_handle_and_escalation_approves() {
        let seams = ScriptSeams::settled(json!({
            "exitCode": 0, "signal": null, "timedOut": false, "aborted": false,
            "timeoutMs": 50.0,
            "stdout": { "text": "", "truncated": false },
            "stderr": { "text": "", "truncated": false },
        }));
        let args = json!({
            "command": "make",
            "description": "Build",
            "run_in_background": true,
            "sandbox_permissions": "danger-full-access",
            "justification": "the build needs the network",
        });
        let value = execute(
            ShellVariant::Bash,
            &seams,
            &registry(),
            &args,
            "c9",
            "c9",
            Some("s1"),
        )
        .expect("execute");
        assert_eq!(
            value,
            json!({ "kind": "background", "jobId": "bash-1", "cwd": "/ws" })
        );
        let (started, call) = &seams.started.lock().expect("started")[0];
        assert!(started.starts_with("bash:make"), "got {started}");
        assert_eq!(call, "c9");
    }

    /// The value validation refusals ride the dsh texts verbatim.
    #[test]
    fn validation_refusals_pin_the_texts() {
        let seams = ScriptSeams::settled(Value::Null);
        let empty = json!({ "command": "  ", "description": "ok" });
        assert_eq!(
            execute(
                ShellVariant::Bash,
                &seams,
                &registry(),
                &empty,
                "c",
                "c",
                None
            )
            .expect_err("refused")
            .message,
            "invalid command: expected a non-empty string"
        );
        let bad_timeout = json!({ "command": "x", "description": "ok", "timeoutMs": 0 });
        assert_eq!(
            execute(
                ShellVariant::Bash,
                &seams,
                &registry(),
                &bad_timeout,
                "c",
                "c",
                None
            )
            .expect_err("refused")
            .message,
            "invalid timeoutMs: expected a positive number, got 0"
        );
        let orphan = json!({ "command": "x", "description": "ok", "sandbox_permissions": "danger-full-access" });
        assert_eq!(
            execute(
                ShellVariant::Bash,
                &seams,
                &registry(),
                &orphan,
                "c",
                "c",
                Some("s1")
            )
            .expect_err("refused")
            .message,
            "invalid escalation: sandbox_permissions requires a justification"
        );
    }

    /// The twin presentations: the pending call cards and the completed
    /// result cards.
    #[test]
    fn presentations_pin_the_card_shapes() {
        let foreground_args = json!({ "command": "ls", "description": "List", "workdir": "/w" });
        assert_eq!(
            present_call(&foreground_args),
            json!({ "card": "terminal", "title": "ls", "description": "List", "cwd": "/w" })
        );
        let background_args =
            json!({ "command": "make", "description": "Build", "run_in_background": true });
        assert_eq!(
            present_call(&background_args),
            json!({
                "card": "generic",
                "title": "make",
                "kind": "execute",
                "rawInput": "make",
                "content": [{ "type": "text", "text": "Build" }],
            })
        );

        let done = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "out\n[exit code: 3]" }],
        });
        assert_eq!(
            present_result(&foreground_args, &done),
            Some(json!({ "card": "terminal", "output": "out", "exitCode": 3 }))
        );
        let clean = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "out\n" }],
        });
        assert_eq!(
            present_result(&foreground_args, &clean),
            Some(json!({ "card": "terminal", "output": "out\n" }))
        );
        let signal_done = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "\n[killed by signal: SIGTERM]" }],
        });
        assert_eq!(
            present_result(&foreground_args, &signal_done),
            Some(json!({ "card": "terminal", "output": "", "signal": "SIGTERM" }))
        );
        let background_done = json!({
            "isError": false,
            "content": [{ "type": "text", "text": "started background job bash-1" }],
        });
        assert_eq!(
            present_result(&background_args, &background_done),
            Some(json!({
                "card": "generic",
                "content": [{ "type": "text", "text": "```console\nstarted background job bash-1\n```" }],
            }))
        );
        let promoted_done = json!({
            "isError": false,
            "value": { "kind": "promoted" },
            "content": [{ "type": "text", "text": "body" }],
        });
        assert_eq!(
            present_result(&foreground_args, &promoted_done)
                .and_then(|view| view.get("card").cloned()),
            Some(json!("generic"))
        );
        // A multi-block result declines the projection.
        let multi = json!({
            "isError": false,
            "content": [
                { "type": "text", "text": "a" },
                { "type": "text", "text": "b" },
            ],
        });
        assert_eq!(present_result(&foreground_args, &multi), None);
    }
}
