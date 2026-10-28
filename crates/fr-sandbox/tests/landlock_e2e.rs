// Rust guideline compliant 2026-10-29
//! Real-confinement E2E over the built launcher binary (the batch 2b
//! host facts: this dev host carries no bwrap, so the chain degrades to
//! Landlock, and a 6.12 kernel enforces Landlock ABI 5 — the launcher
//! probe decides which arm the test may pin).

use std::process::Command;

use fr_sandbox::provider::LocalSandboxProvider;
use fr_sandbox::provider::ProviderConfig;
use fr_sandbox::provider::SandboxInternals;
use fr_sandbox::seam::ConfinedMode;
use fr_sandbox::seam::SandboxEnforcement;
use fr_sandbox::seam::SandboxPolicy;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_fr-landlock-run");

/// The launcher's probe verdict on this host (`None` = unusable).
fn probe_verdict() -> Option<&'static str> {
    let output = Command::new(LAUNCHER)
        .arg("--probe")
        .output()
        .expect("probe spawn");
    if !output.status.success() {
        assert_eq!(output.status.code(), Some(125), "a refused probe exits 125");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("landlock-run: "),
            "the refusal carries the launcher signature: {stderr}"
        );
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("partially enforced") {
        Some("partial")
    } else {
        assert!(
            stdout.contains("landlock: fully enforced"),
            "the probe report line is the contract: {stdout}"
        );
        Some("full")
    }
}

/// The provider with the real Landlock probe and a failing bwrap rung
/// (the host has no bwrap — the degradation arm is the real one).
fn degraded_provider() -> LocalSandboxProvider {
    LocalSandboxProvider::new(
        &ProviderConfig::default(),
        SandboxInternals {
            probe_bwrap: Some(std::sync::Arc::new(|| false)),
            landlock_launcher: Some(LAUNCHER.to_owned()),
            ..SandboxInternals::default()
        },
    )
    .expect("provider")
}

fn policy(root: &str) -> SandboxPolicy {
    SandboxPolicy {
        mode: ConfinedMode::WorkspaceWrite,
        workspace_root: root.to_owned(),
        session_id: None,
    }
}

#[test]
fn the_probe_contract_holds_on_this_host() {
    // Whatever the verdict, the contract shape is pinned; the dev host
    // is expected to enforce (kernel 6.12, ABI 5).
    let verdict = probe_verdict();
    assert!(
        verdict.is_some(),
        "this E2E suite requires an enforcing kernel; probe returned unusable"
    );
}

#[test]
fn bwrap_missing_degrades_to_the_landlock_rung() {
    if probe_verdict().is_none() {
        // Unenforcing kernel: the chain verdict itself must fail closed
        // (the provider refuses to confine, never passes through).
        let refusal = degraded_provider()
            .confine(&["true".to_owned()], &policy("/tmp"))
            .unwrap_err();
        assert!(
            refusal.to_string().contains("no sandbox backend is usable"),
            "the refusal is fail-closed: {refusal}"
        );
        return;
    }
    let confined = degraded_provider()
        .confine(&["true".to_owned()], &policy("/tmp"))
        .expect("the chain degrades to the launcher");
    assert_eq!(confined.argv.first().map(String::as_str), Some(LAUNCHER));
    assert_eq!(confined.enforcement, SandboxEnforcement::Full);
    assert_eq!(confined.denial_signatures, ["permission denied".to_owned()]);
    // The enforcement report must match the probe's own verdict.
    let expected = match probe_verdict() {
        Some("partial") => SandboxEnforcement::Partial,
        _ => SandboxEnforcement::Full,
    };
    assert_eq!(confined.enforcement, expected);
}

#[test]
fn a_write_outside_the_workspace_is_denied_and_inside_passes() {
    if probe_verdict().is_none() {
        return; // Unenforcing kernel: the confinement arms cannot run.
    }
    let workspace = std::env::temp_dir().join("fr-sandbox-e2e-ws");
    // The "outside" tree must sit beyond EVERY writable root the
    // workspace-write profile grants: the workspace, the host /tmp, and
    // the platform temp dir all legitimate writes — so this parks it
    // under the home directory.
    let home = std::env::var("HOME").expect("HOME for the outside tree");
    let outside = std::path::Path::new(&home).join("fr-sandbox-e2e-out");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::create_dir_all(&outside).expect("outside dir");
    let workspace = workspace.to_string_lossy().into_owned();
    let outside_file = outside.join("denied.txt");
    let _ignored = std::fs::remove_file(&outside_file);

    let confined = degraded_provider()
        .confine(
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                format!("echo x > {}", outside_file.to_string_lossy()),
            ],
            &policy(&workspace),
        )
        .expect("wrapped");
    let status = Command::new(&confined.argv[0])
        .args(&confined.argv[1..])
        .status()
        .expect("confined spawn");
    assert!(
        !status.success(),
        "a write outside the workspace must not succeed"
    );
    assert!(!outside_file.exists(), "the denied write left no residue");

    let inside_file = std::path::Path::new(&workspace).join("allowed.txt");
    let confined = degraded_provider()
        .confine(
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                format!("echo x > {}", inside_file.to_string_lossy()),
            ],
            &policy(&workspace),
        )
        .expect("wrapped");
    let status = Command::new(&confined.argv[0])
        .args(&confined.argv[1..])
        .status()
        .expect("confined spawn");
    assert!(status.success(), "a write inside the workspace passes");
    assert!(inside_file.exists(), "the allowed write landed");

    let _ignored = std::fs::remove_file(&inside_file);
    let _ignored = std::fs::remove_file(&outside_file);
}

#[test]
fn the_launcher_usage_contract_pins_its_fatal_shapes() {
    // No command after `--`.
    let output = Command::new(LAUNCHER).arg("--ro").output().expect("spawn");
    assert_eq!(output.status.code(), Some(125));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--ro requires a path"),
        "the usage refusal is verbatim"
    );
    // Unknown argument.
    let output = Command::new(LAUNCHER)
        .args(["--bogus"])
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(125));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown argument: --bogus"),
        "the unknown-argument refusal is verbatim"
    );
    // An unopenable grant root fails closed without running the command.
    let output = Command::new(LAUNCHER)
        .args(["--ro", "/no/such/root/anywhere", "--", "true"])
        .output()
        .expect("spawn");
    assert_eq!(output.status.code(), Some(125));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cannot open rule path: /no/such/root/anywhere"),
        "the unopenable-root refusal is verbatim"
    );
}
