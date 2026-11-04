//! The host boot assembly face (装配器).
//!
//! One boot chain: fr-config `load_stack` (the same resolution path as the
//! dump arms — PRD 6.1 layer order, evaluated parse) → compose → manifest
//! merge → entry-facts injection → `compile_to_loader_doc` (fail-closed
//! per-slot exclusion; the entry row unresolvable is fatal, the reserved
//! rows and the four future crates are pending-slice warnings) → dougong
//! loader `Reconciler` over real cdylib artifacts → bounded join of the
//! whole tree → boot-window facts relay → event pump + signal serve.
//!
//! Fatal boot errors flow to the startup diagnostics face
//! (`crate::diagnostics::report`) and exit 1 (headless spec decision line;
//! dsh `bin.ts:43-47` + `startup-diagnostics.ts` twin).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use dougong::{Context, Invoker};
use dougong_loader::{Config, Reconciler};
use fr_config::{
    EntryKind, EvalContext, FlatRow, PathResolver, StackRequest, compile_to_loader_doc, compose,
    load_stack, schema_lookup,
};
use serde_json::json;

use crate::diagnostics::BootFatal;
use crate::manifest;
use crate::pump;
use crate::signals;

/// One parsed boot invocation (the fr shape of dsh's profile boot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootInvocation {
    /// The selected profile name.
    profile: String,
    /// Factory template replacing the profile before loading (dsh
    /// `--from-default-profile`).
    from_default_profile: Option<String>,
    /// Required `--patch` overlay files.
    patches: Vec<PathBuf>,
    /// The app argv snapshot (everything the launcher did not own).
    app_argv: Vec<String>,
}

impl BootInvocation {
    /// Builds an invocation from the launcher's parse result; the app argv
    /// starts empty and the launcher appends the verbatim rest.
    #[must_use]
    pub fn new(
        profile: impl Into<String>,
        from_default_profile: Option<String>,
        patches: Vec<PathBuf>,
    ) -> Self {
        Self {
            profile: profile.into(),
            from_default_profile,
            patches,
            app_argv: Vec::new(),
        }
    }

    /// The verbatim app argv snapshot.
    #[must_use]
    pub fn app_argv(&self) -> &[String] {
        &self.app_argv
    }

    /// Records the app argv (the launcher's shorthand expansion result).
    pub fn set_app_argv(&mut self, argv: Vec<String>) {
        self.app_argv = argv;
    }

    /// The selected profile name.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// The selected overlay patches.
    #[must_use]
    pub fn patches(&self) -> &[PathBuf] {
        &self.patches
    }

    /// The factory template replacement (dsh `--from-default-profile`).
    #[must_use]
    pub fn from_default_profile(&self) -> Option<&str> {
        self.from_default_profile.as_deref()
    }
}

/// The boot deadline: the longest the tree join may take before the boot
/// names the parked slots and fails closed. Local activations are
/// millisecond-scale; the bound only bites a genuinely broken composition.
const BOOT_JOIN_BOUND: Duration = Duration::from_secs(5);

/// The bounded shutdown grace (dsh `PROCESS_SHUTDOWN_TIMEOUT_MS` twin).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// One compiled document stage: the typed loader config plus the per-row
/// compile failures (the reserved rows and the four future crates).
#[derive(Debug)]
pub struct CompileStage {
    /// The typed loader document.
    pub config: Config,
    /// Per-row compile failures (reserved rows and the four future crates).
    pub failures: Vec<fr_config::CompileFailure>,
    /// The number of slots that resolved into the document.
    pub slot_count: usize,
}

/// The entry-facts injection target: the headless bundle's entry row id.
const ENTRY_ROW_ID: &str = "headless-entry";

/// Compiles the composition into the loader document: fr-config layer
/// stack → compose → manifest merge → entry-facts injection →
/// `compile_to_loader_doc` → typed loader config.
///
/// # Errors
///
/// Fatal fr-config errors (bad profile, unreadable patch files) and a
/// fatal entry row (unresolved artifact or the row dropped from the
/// composition) return [`BootFatal`]; the reserved rows and the four
/// future crates surface as per-row compile failures on the stage.
///
/// # Panics
///
/// Panics only if `serde_json` cannot serialize a bundle name (never for
/// string slices).
pub fn compile_stage(
    invocation: &BootInvocation,
    home: &std::path::Path,
    ctx: &EvalContext,
) -> Result<CompileStage, BootFatal> {
    let fatal = |summary: String| BootFatal {
        detail: summary.clone(),
        summary,
    };
    let outcome = load_stack(
        "fr",
        home,
        StackRequest {
            entry: EntryKind::Headless,
            profile: Some(invocation.profile()),
            overlays: invocation.patches(),
            from_default_profile: invocation.from_default_profile(),
        },
        &|message| eprintln!("{message}"),
        ctx,
    )
    .map_err(|error| fatal(format!("fr: cannot assemble the profile: {error}")))?;
    for bundle in &outcome.skipped_bundles {
        eprintln!(
            "fr: skipping profile bundle {}: {}",
            serde_json::to_string(bundle.name.as_str())
                .expect("serde_json cannot fail to serialize a string"),
            bundle.reason
        );
    }
    let composed = compose(&outcome.layers);
    let mut rows = composed.rows;
    // Manifest merge: bundle/profile rows win; the manifest appends only
    // names the composition lacks (dependencies first, order-insensitive
    // beyond that — the kernel park-and-converges).
    let named: std::collections::HashSet<String> =
        rows.iter().map(|row| row.name.clone()).collect();
    for row in manifest::manifest_rows() {
        if !named.contains(row.name) {
            rows.push(FlatRow {
                id: row.id.to_owned(),
                name: row.name.to_owned(),
                config: row.config.clone(),
                disabled: false,
            });
        }
    }
    for row in manifest::reserved_rows() {
        rows.push(FlatRow {
            id: row.id.to_owned(),
            name: row.name.to_owned(),
            config: row.config.clone(),
            disabled: false,
        });
    }
    // Entry facts: bake the app argv snapshot into the entry row config
    // (headless spec decision line; the service relay is the reading rail
    // — dougong dylib activations receive no slot config).
    let entry_row = rows
        .iter_mut()
        .find(|row| row.id == ENTRY_ROW_ID && !row.disabled)
        .ok_or_else(|| {
            fatal("fr: the headless entry row is missing from the composition".to_owned())
        })?;
    entry_row.config = json!({
        "app": { "argv": invocation.app_argv() }
    });
    let compiled = compile_to_loader_doc(
        &rows,
        &resolver(home)?,
        &manifest::is_first_party,
        &schema_lookup,
    );
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(error) => {
            return Err(fatal(format!("fr: cannot compile the assembly: {error}")));
        }
    };
    for failure in &compiled.failures {
        // The entry row's own failure is fatal; every other unresolved row
        // is a pending-slice warning (decision 16: builtin never; fail-closed
        // per-slot exclusion otherwise).
        if failure.name == "fr-headless" {
            return Err(fatal(format!(
                "fr: the headless entry plugin did not resolve: {}",
                failure.reason
            )));
        }
    }
    for failure in &compiled.failures {
        eprintln!(
            "fr: pending plugin row {}: {}",
            failure.name, failure.reason
        );
    }
    let config = Config::parse(&compiled.doc)
        .map_err(|error| fatal(format!("fr: loader document rejected: {error}")))?;
    let slot_count = config.plugins.len();
    Ok(CompileStage {
        config,
        failures: compiled.failures,
        slot_count,
    })
}

/// The artifact search dirs: `FR_PLUGIN_PATH` entries first, then the
/// executable's directory and its parent (the dev layout `target/debug`
/// and the installed `lib/` layout both resolve).
fn resolver(home: &std::path::Path) -> Result<PathResolver, BootFatal> {
    let _ = home;
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(paths) = std::env::var_os("FR_PLUGIN_PATH") {
        dirs.extend(std::env::split_paths(&paths));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    if dirs.is_empty() {
        return Err(BootFatal {
            summary: "fr: cannot locate plugin artifacts".to_owned(),
            detail:
                "fr: cannot locate plugin artifacts (no FR_PLUGIN_PATH, no executable directory)"
                    .to_owned(),
        });
    }
    Ok(PathResolver::new(dirs))
}

/// The relay method on the entry plugin's own service (the boot-window
/// facts delivery rail, headless spec decision line).
const METHOD_ADOPT_FACTS: u32 = 2;

/// Boots the composition and serves it: compile → kernel → join → facts
/// relay → pump + signals. Runs inside a tokio current-thread runtime.
///
/// # Errors
///
/// Returns [`BootFatal`] when any boot stage fails closed; the caller
/// routes it to the startup diagnostics face and exit 1.
pub async fn boot_and_serve(
    invocation: &BootInvocation,
    home: &std::path::Path,
) -> Result<RunningBoot, BootFatal> {
    let mut live = boot(invocation, home).await?;
    let code = serve_until_shutdown(&mut live).await;
    Ok(code)
}

/// The live boot tree: the kernel context, the reconciler owning the
/// fibers, and the running pump task.
#[derive(Debug)]
pub struct LiveTree {
    /// The kernel root context.
    pub ctx: Context,
    /// The reconciler holding every live slot.
    pub reconciler: Reconciler,
    /// The host pump task handle.
    pub pump: tokio::task::JoinHandle<()>,
}

/// Boots the composition without serving: compile → kernel → join →
/// facts relay.
///
/// # Errors
///
/// Returns [`BootFatal`] when any boot stage fails closed.
pub async fn boot(
    invocation: &BootInvocation,
    home: &std::path::Path,
) -> Result<LiveTree, BootFatal> {
    let fatal = |summary: String| BootFatal {
        detail: summary.clone(),
        summary,
    };
    let ctx = EvalContext::process(true);
    let stage = compile_stage(invocation, home, &ctx)?;
    let _ = stage.slot_count;
    let _ = stage.failures;
    let _ = stage.config;
    let kernel_ctx = Context::new();
    let mut reconciler = Reconciler::new(HashMap::new(), None);
    reconciler.replace(stage.config);
    let report = reconciler.apply(&kernel_ctx).await;
    if !report.failures.is_empty() {
        let names: Vec<String> = report
            .failures
            .iter()
            .map(|(name, error)| format!("{name}: {error}"))
            .collect();
        return Err(fatal(format!(
            "fr: loader rejected the boot: {}",
            names.join("; ")
        )));
    }
    let deadline = tokio::time::Instant::now() + BOOT_JOIN_BOUND;
    join_tree(&reconciler).await?;
    rearm_parked(&reconciler, deadline).await?;
    relay_facts(&kernel_ctx, invocation).await?;
    let pump = pump::spawn(kernel_ctx.clone());
    Ok(LiveTree {
        ctx: kernel_ctx,
        reconciler,
        pump,
    })
}

/// Bounded join of the whole live tree: any timeout or activation
/// failure fails the boot closed.
///
/// # Errors
///
/// Returns [`BootFatal`] when a slot fails to activate within
/// [`BOOT_JOIN_BOUND`]; parked slots are left to [`rearm_parked`].
async fn join_tree(reconciler: &Reconciler) -> Result<(), BootFatal> {
    let deadline = tokio::time::Instant::now() + BOOT_JOIN_BOUND;
    for (name, entry) in reconciler.live() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        entry
            .handle
            .join_with_timeout(remaining)
            .await
            .map_err(|error| BootFatal {
                summary: format!("fr: slot {name} did not activate: {error}"),
                detail: format!("fr: slot {name} did not activate within the boot bound: {error}"),
            })?;
    }
    Ok(())
}

/// One deterministic re-arm sweep: a parked slot whose provider has since
/// committed may have missed the commit wake; `restart` re-runs the
/// inject checks against the current committed view. Only the first
/// sweep retries — a slot still parked afterwards is a real composition
/// gap, not a wake race (2026-11-04 first-co-load finding).
///
/// # Errors
///
/// Returns [`BootFatal`] when a re-armed slot still does not activate.
async fn rearm_parked(
    reconciler: &Reconciler,
    deadline: tokio::time::Instant,
) -> Result<(), BootFatal> {
    let parked: Vec<String> = reconciler
        .live()
        .iter()
        .filter(|(_, entry)| entry.handle.state() != dougong::FiberState::Active)
        .map(|(name, _)| name.clone())
        .collect();
    for name in parked {
        let entry = &reconciler.live()[&name];
        let _restarted = entry.handle.restart().await;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        entry
            .handle
            .join_with_timeout(remaining)
            .await
            .map_err(|error| BootFatal {
                summary: format!("fr: slot {name} did not activate: {error}"),
                detail: format!("fr: slot {name} did not activate within the boot bound: {error}"),
            })?;
        let state = entry.handle.state();
        if state != dougong::FiberState::Active {
            return Err(BootFatal {
                summary: format!("fr: slot {name} did not activate (state {state:?})"),
                detail: format!(
                    "fr: slot {name} stayed {state:?} after the re-arm sweep — a declared inject never committed"
                ),
            });
        }
    }
    Ok(())
}

/// Delivers the app argv snapshot through the entry plugin's own service
/// (the sanctioned reading rail: dougong dylib activations receive no
/// slot config; headless spec decision line, 2026-11-04 amendment).
///
/// # Errors
///
/// Fails closed when the entry service is missing or refuses the facts.
async fn relay_facts(ctx: &Context, invocation: &BootInvocation) -> Result<(), BootFatal> {
    let fatal = |summary: String| BootFatal {
        detail: summary.clone(),
        summary,
    };
    let facts = json!({ "app": { "argv": invocation.app_argv() } });
    let bytes = serde_json::to_vec(&facts)
        .map_err(|error| fatal(format!("fr: cannot serialize the entry facts: {error}")))?;
    let invoker = ctx
        .require::<Invoker>("headless-startup")
        .map_err(|error| fatal(format!("fr: entry facts relay failed: {error}")))?;
    invoker
        .call(METHOD_ADOPT_FACTS, bytes)
        .await
        .map_err(|error| fatal(format!("fr: entry facts relay failed: {error}")))?;
    Ok(())
}

/// The serve loop outcome: the shutdown exit code (dsh profile-boot.ts:307-308
/// twin: SIGINT 130, SIGTERM 0).
#[derive(Debug)]
pub struct RunningBoot {
    /// The process exit code the launcher maps to `ExitCode`.
    exit_code: u32,
}

impl RunningBoot {
    /// The shutdown exit code (SIGINT 130, SIGTERM 0).
    #[must_use]
    pub const fn exit_code(&self) -> u32 {
        self.exit_code
    }
}

/// Serves until shutdown, then hands back the mapped exit code.
///
/// The process face wraps [`shutdown_tree`] with the dsh force-exit
/// watchdog (`setTimeout(forceExitOnce)` twin): a teardown join blocking
/// the single runtime thread cannot stop the OS timer.
///
/// # Panics
///
/// Panics only if the watchdog thread cannot spawn (thread-exhaustion
/// level failure; nothing the boot can recover).
pub async fn serve_until_shutdown(live: &mut LiveTree) -> RunningBoot {
    let reason = signals::wait_shutdown().await;
    let code = reason.exit_code();
    let watchdog = std::thread::Builder::new()
        .name("fr-shutdown-watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(SHUTDOWN_GRACE);
            std::process::exit(code.cast_signed());
        })
        .expect("the shutdown watchdog spawns");
    let _ = shutdown_tree(live, reason).await;
    let _ = watchdog.join();
    // The graceful dispose converged (or the watchdog already fired).
    std::process::exit(code.cast_signed());
}

/// The host-owned shutdown: quiesce the pump, then broadcast (retire the
/// whole tree) under the dsh grace. Returns the mapped exit code without
/// exiting the process (the caller decides).
///
/// The kernel disposes in reverse registration order and joins every
/// plugin-owned thread. The retire runs as a detached task: a converge
/// stall (dispose deferrals) cannot pin the caller past the grace, and
/// the leaked task dies with the process.
///
/// # Panics
///
/// Never in practice; the retire task spawn panics only without a
/// runtime.
pub async fn shutdown_tree(live: &mut LiveTree, reason: signals::ShutdownReason) -> u32 {
    let code = reason.exit_code();
    // Quiesce the host pump first: its 50ms drain requires race the
    // retire's dispose chain on the target plugins' instance locks, and a
    // tick landing mid-dispose can stall the retire (2026-11-04
    // shutdown-hang finding).
    live.pump.abort();
    let ctx = live.ctx.clone();
    let mut reconciler = std::mem::replace(
        &mut live.reconciler,
        Reconciler::new(std::collections::HashMap::new(), None),
    );
    reconciler.replace(Config {
        plugins: Vec::new(),
    });
    let retire = tokio::spawn(async move {
        let _retired = reconciler.apply(&ctx).await;
    });
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, retire).await;
    code
}
