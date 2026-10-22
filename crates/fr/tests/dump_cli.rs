//! Process-level tests for the `fr` dump CLI surface (PRD 23 decision 27).
//!
//! Each test spawns the built binary (`CARGO_BIN_EXE_fr`) against an
//! isolated `FR_HOME`, asserting exit codes and the stdout/stderr split as
//! real process behavior.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct TempHome {
    dir: PathBuf,
}

impl TempHome {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fr-bin-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
        Self { dir }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], home: &TempHome) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_fr"))
        .args(args)
        .env("FR_HOME", home.path())
        .output()
        .unwrap_or_else(|error| panic!("spawn failed: {error}"));
    Run {
        status: output
            .status
            .code()
            .unwrap_or_else(|| panic!("fr was killed by a signal")),
        stdout: String::from_utf8(output.stdout).expect("stdout is utf-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is utf-8"),
    }
}

#[test]
fn dump_config_writes_composed_yaml_to_stdout() {
    let home = TempHome::new("dump-happy");
    let run = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert!(run.stderr.is_empty(), "stderr: {}", run.stderr);
    assert!(
        run.stdout.contains("name: fr-agent-loop"),
        "stdout: {}",
        run.stdout
    );
    assert!(run.stdout.ends_with('\n'), "stdout: {}", run.stdout);
}

#[test]
fn schema_dump_streams_json_and_diagnostics() {
    let home = TempHome::new("schema-happy");
    // Initialize the profile first so the schema run's stderr is purely
    // diagnostics (mirrors the dsh built-bin acceptance fixture).
    let init = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(init.status, 0, "stderr: {}", init.stderr);
    let run = run(&["dump-config-schema", "--profile", "headless"], &home);
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert!(run.stderr.is_empty(), "stderr: {}", run.stderr);
    // stdout carries exactly one pretty JSON document plus one newline.
    assert!(run.stdout.ends_with("}\n"), "stdout tail: {}", run.stdout);
    assert!(!run.stdout.ends_with("\n\n"));
    let document: serde_json::Value =
        serde_json::from_str(run.stdout.strip_suffix('\n').expect("trailing newline"))
            .unwrap_or_else(|error| panic!("stdout is not one JSON document: {error}"));
    assert_eq!(
        document.pointer("/x-fr/complete"),
        Some(&serde_json::Value::Bool(true))
    );
}

#[test]
fn incomplete_schema_dump_exits_one() {
    let home = TempHome::new("schema-incomplete");
    let profile_dir = home.path().join("profiles").join("headless");
    fs::create_dir_all(&profile_dir).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
    fs::write(
        profile_dir.join("profile.yml"),
        "bundles: [missing-bundle]\n",
    )
    .unwrap_or_else(|error| panic!("write failed: {error}"));
    let run = run(&["dump-config-schema", "--profile", "headless"], &home);
    assert_eq!(run.status, 1);
    // stdout remains exactly one JSON document; stderr carries the prepare-time
    // skipped-bundle report (dsh reportSkippedBundles) and then the schema
    // diagnostic with the path-less `fr: <level>: <message>` spelling (decision 25).
    let document: serde_json::Value =
        serde_json::from_str(run.stdout.strip_suffix('\n').expect("trailing newline"))
            .unwrap_or_else(|error| panic!("stdout is not one JSON document: {error}"));
    assert_eq!(
        document.pointer("/x-fr/complete"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        run.stderr,
        "fr: skipping profile bundle \"missing-bundle\": not a built-in bundle name\n\
         fr: error: Selected profile bundle \"missing-bundle\" could not be loaded; repair or remove its bundle selection.\n"
    );
}

#[test]
fn dump_rejects_app_arguments_and_unknown_flags() {
    let home = TempHome::new("reject-inner");
    let invocations: [&[&str]; 4] = [
        &["dump-config", "--profile", "headless", "task"],
        &["dump-config-schema", "--profile", "headless", "--bogus"],
        &["dump-config", "--profile", "headless", "-h"],
        &[
            "dump-config",
            "--profile",
            "headless",
            "--patch",
            "a.yml",
            "late",
            "--patch",
            "b.yml",
        ],
    ];
    for invocation in invocations {
        let run = run(invocation, &home);
        assert_eq!(run.status, 1, "argv {invocation:?}");
        assert!(
            run.stdout.is_empty(),
            "argv {invocation:?} stdout: {}",
            run.stdout
        );
        assert!(
            run.stderr
                .starts_with("error: config dumps take no app arguments, got "),
            "argv {invocation:?} stderr: {}",
            run.stderr
        );
    }
    // The got-list quotes each argument the way dsh JSON.stringify does.
    let run = run(
        &["dump-config-schema", "--profile", "headless", "--bogus"],
        &home,
    );
    assert_eq!(
        run.stderr,
        "error: config dumps take no app arguments, got \"--bogus\"\n"
    );
}

#[test]
fn dump_arg_validation_error_family() {
    let home = TempHome::new("arg-errors");
    let cases: [(&[&str], &str); 10] = [
        (&["dump-config"], "error: --profile <name> is required\n"),
        (
            &["dump-config-schema"],
            "error: --profile <name> is required\n",
        ),
        (
            &["dump-config", "--profile", ""],
            "error: --profile needs a name\n",
        ),
        (
            &["dump-config", "--profile", "a", "--profile", "b"],
            "error: select a profile only once\n",
        ),
        (
            &["dump-config", "--profile", "headless", "--patch="],
            "error: --patch needs a path\n",
        ),
        (
            &[
                "dump-config",
                "--profile",
                "headless",
                "--from-default-profile=",
            ],
            "error: --from-default-profile needs a name\n",
        ),
        (
            &[
                "dump-config",
                "--profile",
                "headless",
                "--default-only",
                "--patch",
                "x.yml",
            ],
            "error: --default-only prints the bundle layers and takes no --patch\n",
        ),
        // An option at the end of argv with no value takes commander's own
        // wording, distinct from the empty-value family above.
        (
            &["dump-config", "--profile"],
            "error: option '--profile <name>' argument missing\n",
        ),
        (
            &["dump-config", "--profile", "headless", "--patch"],
            "error: option '--patch <path>' argument missing\n",
        ),
        (
            &["dump-config", "--from-default-profile"],
            "error: option '--from-default-profile <name>' argument missing\n",
        ),
    ];
    for (argv, expected) in cases {
        let run = run(argv, &home);
        assert_eq!(run.status, 1, "argv {argv:?}");
        assert_eq!(run.stderr, expected, "argv {argv:?}");
        assert!(run.stdout.is_empty(), "argv {argv:?}");
    }
}

#[test]
fn dump_fatal_error_prints_single_line_and_exits_one() {
    let home = TempHome::new("fatal-overlay");
    let init = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(init.status, 0, "stderr: {}", init.stderr);
    let run = run(
        &[
            "dump-config",
            "--profile",
            "headless",
            "--patch",
            "missing.yml",
        ],
        &home,
    );
    assert_eq!(run.status, 1);
    assert!(run.stdout.is_empty(), "stdout: {}", run.stdout);
    // One stderr line carrying the overlay diagnostic (the fr equivalent of
    // dsh's unhandled throw), never a multi-line report.
    let lines: Vec<&str> = run.stderr.lines().collect();
    assert_eq!(lines.len(), 1, "stderr: {}", run.stderr);
    assert!(lines[0].contains("missing.yml"), "stderr: {}", run.stderr);
}

#[test]
fn default_only_dumps_broken_profile_at_cli() {
    let home = TempHome::new("default-only-cli");
    let init = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(init.status, 0, "stderr: {}", init.stderr);
    let patch = home
        .path()
        .join("profiles")
        .join("headless")
        .join("fr.patch.yml");
    fs::write(&patch, "{{{ not yaml\n").unwrap_or_else(|error| panic!("write failed: {error}"));
    let broken = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(broken.status, 1, "stderr: {}", broken.stderr);
    assert!(broken.stdout.is_empty(), "stdout: {}", broken.stdout);
    // --default-only is the recovery diagnostic: the broken user layer is
    // never read, and the bundle layers still dump.
    let recovered = run(
        &["dump-config", "--profile", "headless", "--default-only"],
        &home,
    );
    assert_eq!(recovered.status, 0, "stderr: {}", recovered.stderr);
    assert!(
        recovered.stdout.contains("name: fr-agent-loop"),
        "stdout: {}",
        recovered.stdout
    );
}

#[test]
fn root_dispatch_version_help_and_unknown_modes() {
    let home = TempHome::new("root-face");
    let version = run(&["--version"], &home);
    assert_eq!(version.status, 0);
    assert_eq!(version.stdout, concat!(env!("CARGO_PKG_VERSION"), "\n"));
    assert_eq!(version.stderr, "");
    assert_eq!(run(&["-V"], &home).status, 0);
    let help = run(&["--help"], &home);
    assert_eq!(help.status, 0);
    assert!(help.stdout.contains("Usage:"));
    assert!(help.stdout.contains("fr dump-config-schema"));
    assert_eq!(run(&["-h"], &home).status, 0);
    let bare = run(&[], &home);
    assert_eq!(bare.status, 1);
    assert_eq!(bare.stdout, "");
    assert!(bare.stderr.starts_with("error: expected a mode"));
    let unknown = run(&["headless"], &home);
    assert_eq!(unknown.status, 1);
    assert!(unknown.stderr.starts_with("error: expected a mode"));
}

#[test]
fn subcommand_version_help_interception() {
    let home = TempHome::new("subcommand-face");
    // The launcher owns -V/--version anywhere before the first app token
    // (commander fires the version during parse, even mid-flag region).
    let version = run(&["dump-config", "-V"], &home);
    assert_eq!(version.status, 0);
    assert_eq!(version.stdout, concat!(env!("CARGO_PKG_VERSION"), "\n"));
    assert_eq!(version.stderr, "");
    let version_long = run(&["dump-config-schema", "--version"], &home);
    assert_eq!(version_long.status, 0);
    assert_eq!(
        version_long.stdout,
        concat!(env!("CARGO_PKG_VERSION"), "\n")
    );
    // With no profile to hand it to, -h/--help prints the launcher's own
    // help (dsh args.ts intercepts before the profile-required error).
    let help = run(&["dump-config", "-h"], &home);
    assert_eq!(help.status, 0);
    assert!(help.stdout.contains("Usage:"));
    assert_eq!(help.stderr, "");
    let help_long = run(&["dump-config-schema", "--help"], &home);
    assert_eq!(help_long.status, 0);
    assert!(help_long.stdout.contains("Usage:"));
    // But with a profile named, -h is an app argument like any other.
    let owned = run(&["dump-config", "--profile", "headless", "-h"], &home);
    assert_eq!(owned.status, 1);
    assert_eq!(
        owned.stderr,
        "error: config dumps take no app arguments, got \"-h\"\n"
    );
}

#[test]
fn end_of_options_marker_is_consumed() {
    let home = TempHome::new("end-of-options");
    let init = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(init.status, 0, "stderr: {}", init.stderr);
    // A bare `--` is commander's end-of-options literal: it is consumed, so
    // the dump proceeds where the pre-fix parser rejected it as an app argument.
    let bare = run(&["dump-config", "--profile", "headless", "--"], &home);
    assert_eq!(bare.status, 0, "stderr: {}", bare.stderr);
    assert!(
        bare.stdout.contains("name: fr-agent-loop"),
        "stdout: {}",
        bare.stdout
    );
    // With app arguments after the marker, the got-list carries those
    // arguments without the marker itself.
    let with_args = run(
        &["dump-config", "--profile", "headless", "--", "--bogus"],
        &home,
    );
    assert_eq!(with_args.status, 1);
    assert_eq!(
        with_args.stderr,
        "error: config dumps take no app arguments, got \"--bogus\"\n"
    );
}

#[test]
fn yaml_dump_reports_skipped_bundle() {
    let home = TempHome::new("yaml-skipped");
    let profile_dir = home.path().join("profiles").join("headless");
    fs::create_dir_all(&profile_dir).unwrap_or_else(|error| panic!("mkdir failed: {error}"));
    fs::write(
        profile_dir.join("profile.yml"),
        "bundles: [missing-bundle]\n",
    )
    .unwrap_or_else(|error| panic!("write failed: {error}"));
    // The YAML dump prints the prepare-time skipped-bundle report to stderr
    // and still dumps the composed tree (dsh runDumpConfig / reportSkippedBundles).
    let run = run(&["dump-config", "--profile", "headless"], &home);
    assert_eq!(run.status, 0, "stderr: {}", run.stderr);
    assert_eq!(
        run.stderr,
        "fr: skipping profile bundle \"missing-bundle\": not a built-in bundle name\n"
    );
    assert!(
        run.stdout.contains("name: fr-agent-loop"),
        "stdout: {}",
        run.stdout
    );
}
