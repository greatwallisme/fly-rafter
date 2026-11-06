//! Integration tests: the 18a host assembly face over real cdylib
//! artifacts (headless spec completion-criteria scenarios).
//!
//! The artifact strategy (2026-11-04 ruling): the four-gate flow runs
//! `cargo build --workspace` first; these tests fail loud with the
//! instruction when the artifacts are missing.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use dougong::Invoker;
use fr::assembly::{BootInvocation, boot, compile_stage, shutdown_tree};
use fr_config::EvalContext;
use serde_json::Value;

/// The verbatim dsh warning header the startup diagnostics carry.
const WARNING_HEADER: &str = "WARNING: Raw diagnostics may contain configuration or credential values from plugin errors. Review before sharing.";

/// Every service the live headless composition provides, keyed by its
/// provider crate (the descriptor truth from the closure survey).
const EXPECTED_SERVICES: &[&str] = &[
    "net",
    "attachments",
    "storage-domain",
    "llm",
    "token-meter",
    "guard-timeout",
    "guard-repeat-reminder",
    "session-persistence",
    "session-projections",
    "system-prompt",
    "tools",
    "shell",
    "subprocess",
    "jobs",
    "terminals",
    "sandbox",
    "sandbox-policy",
    "permission-presets",
    "approval",
    "compaction",
    "spill-store",
    "builtin-tools",
    "fs",
    "user-questions",
    "plan-mode",
    "session-query",
    "skills",
    "agent",
    "headless-startup",
];

/// Every first-party plugin crate the 18a composition resolves to a real
/// cdylib artifact.
const REQUIRED_ARTIFACTS: &[&str] = &[
    "fr-net",
    "fr-attachment",
    "fr-storage",
    "fr-llm",
    "fr-token-meter",
    "fr-guard-timeout",
    "fr-guard-repeat-reminder",
    "fr-session",
    "fr-system-prompt",
    "fr-tools",
    "fr-shell",
    "fr-sandbox",
    "fr-permission-presets",
    "fr-approval",
    "fr-compaction",
    "fr-tools-builtin",
    "fr-agent-loop",
    "fr-llm-deepseek",
    "fr-headless",
];

/// A throwaway `FR_HOME` (removed on drop).
struct TempHome(PathBuf);

impl TempHome {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fr-18a-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp home created");
        // SAFETY: the harness serializes tests (the runner holds one test
        // at a time in this file's usage); FR_HOME is the deployment
        // channel the plugin activations read their baked roots from —
        // without it they resolve the real home (2026-11-04 finding).
        unsafe { std::env::set_var("FR_HOME", &dir) };
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The artifact search dirs (the assembly resolver twin).
fn artifact_dirs() -> Vec<PathBuf> {
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
    dirs
}

/// Fails loud with the build instruction when any cdylib artifact is
/// missing from every search dir (never a silent skip).
fn require_artifacts() {
    let dirs = artifact_dirs();
    let missing: Vec<&str> = REQUIRED_ARTIFACTS
        .iter()
        .filter(|name| {
            let slug = name.replace('-', "_");
            !dirs
                .iter()
                .any(|dir| dir.join(format!("lib{slug}.so")).exists())
        })
        .copied()
        .collect();
    assert!(
        missing.is_empty(),
        "the first-party cdylib artifacts are missing ({missing:?}); \
         run `cargo build --workspace` before `cargo test` (2026-11-04 artifact ruling)"
    );
}

/// The test boot invocation: the headless profile, no overlays, the app
/// argv the caller passes.
fn invocation(app: &[&str]) -> BootInvocation {
    let mut invocation = BootInvocation::new("headless", None, Vec::new());
    invocation.set_app_argv(app.iter().map(|arg| (*arg).to_owned()).collect());
    invocation
}

/// 18a scenario: the headless composition boots over real cdylib
/// artifacts, every provided service resolves from the host context, and
/// the host-owned retire shuts the tree down with the SIGTERM-mapped code.
#[test]
fn headless_assembly_boots_real_dylibs() {
    require_artifacts();
    let home = TempHome::new("boot");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let mut live = boot(&invocation(&[]), home.path())
            .await
            .expect("the headless composition boots over real dylibs");
        for service in EXPECTED_SERVICES {
            let resolved = live.ctx.require::<Invoker>(service);
            assert!(
                resolved.is_ok(),
                "service {service} must resolve after boot: {resolved:?}"
            );
        }
        let code = shutdown_tree(&mut live, 0).await;
        assert_eq!(code, 0, "SIGTERM maps to exit 0");
    });
}

/// 18a scenario: every slot of the compiled headless composition is a
/// dylib source (decision 16 red line) and the only unresolved rows are
/// the future crates with their own slices.
#[test]
fn assembly_manifest_has_no_builtin_sources() {
    require_artifacts();
    let home = TempHome::new("manifest");
    let ctx = EvalContext::process(true);
    let stage = compile_stage(&invocation(&[]), home.path(), &ctx)
        .expect("the headless composition compiles");
    assert_eq!(
        stage.config.plugins.len(),
        stage.slot_count,
        "every slot in the document is a resolved row"
    );
    for spec in &stage.config.plugins {
        assert!(
            matches!(spec.source, dougong_loader::Source::Dylib { .. }),
            "slot {} must be a dylib source, got {:?}",
            spec.name,
            spec.source
        );
        assert!(
            spec.isolate.is_none(),
            "18a pins the isolate declaration face as the ambient (null) shape"
        );
    }
    let pending: Vec<&str> = stage
        .failures
        .iter()
        .map(|failure| failure.name.as_str())
        .collect();
    assert_eq!(
        pending,
        vec!["fr-mcp", "fr-ptc", "fr-subagent"],
        // fr-llm-retry and fr-working-directory resolved when the 18c/18d
        // slices built their crates; the remaining three wait for their
        // own slices.
        "only the future-crate rows may fail resolution"
    );
}

/// 18a scenario: the app argv snapshot lands in the entry slot config and
/// reads back through the entry plugin's own service (the sanctioned
/// reading rail).
#[test]
fn entry_facts_injected_through_document_rows() {
    require_artifacts();
    let home = TempHome::new("facts");
    let app = ["--json", "run the tests"];
    let ctx = EvalContext::process(true);
    let stage =
        compile_stage(&invocation(&app), home.path(), &ctx).expect("the composition compiles");
    let entry = stage
        .config
        .plugins
        .iter()
        .find(|spec| spec.name == "headless-entry")
        .expect("the entry slot is in the document");
    assert_eq!(
        entry.config.get("app").and_then(|app| app.get("argv")),
        Some(&serde_json::json!(app)),
        "the app argv snapshot is baked into the entry slot config"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let mut live = boot(&invocation(&app), home.path())
            .await
            .expect("the composition boots");
        let invoker = live
            .ctx
            .require::<Invoker>("headless-startup")
            .expect("the entry service resolves");
        let reply = invoker
            .call(3, b"{}".to_vec())
            .await
            .expect("the facts read back");
        let facts: Value = serde_json::from_slice(&reply).expect("json facts");
        assert_eq!(
            facts,
            serde_json::json!({
                "app": { "argv": app },
                "model": {
                    "provider": "deepseek-official",
                    "model": "deepseek-flash",
                },
            }),
            "the relayed snapshot reads back verbatim (argv + the model selection rail)"
        );
        // The whenIdle contract before disposal (the 退出守望 lane): the
        // argv snapshot doubles as the runner's task, and an in-flight
        // CREATE at retire time parks the realm spawn's child settle
        // behind the teardown join — the run must converge first, exactly
        // as the production serve loop awaits the exit request (the
        // 2026-11-06 18e realm-spawn finding).
        let exit = tokio::time::timeout(std::time::Duration::from_secs(30), live.exit.recv())
            .await
            .expect("the run converges within the grace bound")
            .expect("the exit lane stays open");
        assert_eq!(exit, 1, "the in-process run fails closed without a provider key");
        let code = shutdown_tree(&mut live, 0).await;
        assert_eq!(code, 0);
    });
}

/// 18a scenario: a boot-time fatal error (unparsable `FR_PERMISSION_MODE`
/// declines the fr-sandbox activation) prints the stderr summary, saves
/// the full report under `$FR_HOME/logs/` with the verbatim warning
/// header, and exits 1.
#[test]
fn startup_failure_writes_diagnostic_log() {
    require_artifacts();
    let home = TempHome::new("diag");
    let output = Command::new(env!("CARGO_BIN_EXE_fr"))
        .arg("headless")
        .env("FR_HOME", home.path())
        .env("FR_PERMISSION_MODE", "not-a-mode")
        .output()
        .expect("the fr binary runs");
    assert_eq!(output.status.code(), Some(1), "fatal boot exits 1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did not activate"),
        "the stderr summary names the declined slot: {stderr}"
    );
    assert!(
        stderr.contains("Full diagnostics:"),
        "the stderr names the saved report: {stderr}"
    );
    let logs = std::fs::read_dir(home.path().join("logs")).expect("logs dir exists");
    let names: Vec<String> = logs
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "one startup report saved: {names:?}");
    assert!(names[0].starts_with("startup-"), "report named {names:?}");
    let body =
        std::fs::read_to_string(home.path().join("logs").join(&names[0])).expect("report readable");
    assert!(
        body.starts_with(WARNING_HEADER),
        "the report carries the verbatim warning header"
    );
}

/// 18a scenario: SIGINT maps to exit 130, SIGTERM maps to exit 0, and the
/// host broadcasts (retires the whole tree) before the mapped exit.
#[test]
fn signals_map_to_dsh_exit_codes() {
    assert_signal_exit("SIGTERM", 0);
    assert_signal_exit("SIGINT", 130);
}

/// Boots the real `fr` binary as a child, signals it, and asserts the
/// mapped exit code (signals own teardown throughout the startup window,
/// so the delivery phase does not change the code).
fn assert_signal_exit(signal: &str, expected: i32) {
    require_artifacts();
    // The runner drives a live generation against a dripping SSE hold
    // (the stream stays open), so the child serves until the signal: the
    // operator override lands inside the serve window deterministically
    // (a self-exiting usage-error run would finish before any signal).
    let hold = std::net::TcpListener::bind("127.0.0.1:0").expect("the hold binds");
    let hold_port = hold.local_addr().expect("hold addr").port();
    let _hold_thread = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = hold.accept() {
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: ping\n\n";
            let _ = stream.write_all(head.as_bytes());
            loop {
                std::thread::sleep(Duration::from_millis(50));
                if stream.write_all(b": ping\n\n").is_err() {
                    return;
                }
            }
        }
    });
    let home = TempHome::new("signals");
    let mut child = Command::new(env!("CARGO_BIN_EXE_fr"))
        .arg("headless")
        .arg("--json")
        .arg("say hi")
        .env("FR_HOME", home.path())
        .env("DEEPSEEK_API_KEY", "test-key")
        .env("DEEPSEEK_BASE_URL", format!("http://127.0.0.1:{hold_port}"))
        .spawn()
        .expect("the fr binary spawns");
    // The serve window: the boot settles and the generation holds on the
    // dripping stream; the signal lands inside it.
    std::thread::sleep(Duration::from_millis(1500));
    let delivery = Command::new("kill")
        .arg("-s")
        .arg(signal)
        .arg(child.id().to_string())
        .status()
        .expect("kill runs");
    assert!(delivery.success(), "kill {signal} delivered");
    let status = child.wait().expect("child exits after the signal");
    assert_eq!(
        status.code(),
        Some(expected),
        "{signal} must map to exit {expected}"
    );
}

/// 18a scenario: a published event reaches a subscribed dylib plugin's
/// `on_event` verbatim. The real emitter→listener pair: the host pump
/// relays the fr-session outbound lane onto the kernel bus
/// (`session/created`, payload `{"sessionId"}`) and the
/// fr-permission-presets dylib listens `session/created`, seeding the
/// initial preset into the durable session log — observable through the
/// session read face.
#[test]
fn event_pump_delivers_listened_events() {
    require_artifacts();
    let home = TempHome::new("pump");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let mut live = boot(&invocation(&[]), home.path())
            .await
            .expect("the composition boots (pump included)");
        let persistence = live
            .ctx
            .require::<Invoker>("session-persistence")
            .expect("the session service resolves");
        let created = persistence
            .call(2, b"{}".to_vec())
            .await
            .expect("session create");
        let created: Value = serde_json::from_slice(&created).expect("json create reply");
        let session = created
            .get("sessionId")
            .and_then(Value::as_str)
            .expect("create replies with the session id")
            .to_owned();
        let persistence = live
            .ctx
            .require::<Invoker>("session-persistence")
            .expect("the session service resolves");
        let seeded = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let request = serde_json::json!({ "id": session })
                    .to_string()
                    .into_bytes();
                let Ok(reply) = persistence.call(9, request).await else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                };
                let Ok(log) = serde_json::from_slice::<Value>(&reply) else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                };
                if log.to_string().contains("permission/preset") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            seeded.is_ok(),
            "the pump relay must reach the presets listener (permission/preset seeded)"
        );
        let reply = persistence
            .call(
                9,
                serde_json::json!({ "id": session })
                    .to_string()
                    .into_bytes(),
            )
            .await
            .expect("final read");
        let log: Value = serde_json::from_slice(&reply).expect("json log");
        let events = log
            .get("events")
            .and_then(Value::as_array)
            .expect("the log carries events");
        let types: Vec<&str> = events
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .collect();
        assert!(
            types.contains(&"permission/preset"),
            "seeded events present: {types:?}"
        );
        assert!(
            types.contains(&"sandbox/mode"),
            "sandbox/mode seeded: {types:?}"
        );
        assert!(
            types.contains(&"approval/policy"),
            "approval/policy seeded: {types:?}"
        );
        let code = shutdown_tree(&mut live, 0).await;
        assert_eq!(code, 0);
    });
}
