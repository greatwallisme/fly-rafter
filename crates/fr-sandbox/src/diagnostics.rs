// Rust guideline compliant 2026-10-29
//! Classification of observed sandbox runner failures and file-effect
//! denials (dsh `sandbox/src/diagnostics.ts`).
//!
//! One structural adaptation (fr `std::io` boundary, recorded in the
//! batch 2b findings): Node spawn rejections carry `error.path` and
//! `error.syscall`, and dsh attributes an `EACCES`/`ENOENT` rejection to
//! the runner only when the path equals `argv[0]`. Rust `io::Error`
//! carries neither field, so [`is_runner_spawn_failure`] keeps the
//! conservative preconditions (a supplied runner program plus an
//! independently usable spawn cwd) and matches on the error kind — the
//! same fail-closed direction with a narrower attribution aperture.

use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::seam::RunnerFailureRule;

/// Whether the caller-owned spawn cwd can be entered (dsh
/// `isUsableWorkdir`): an existing directory searchable by this process.
fn is_usable_workdir(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Attributes a spawn rejection to the runner executable (dsh
/// `isRunnerSpawnFailure`, adapted).
///
/// The runner program must be supplied and the caller-owned cwd
/// independently usable, and the rejection must be executable resolution
/// or permission (`NotFound`/`PermissionDenied` — the Rust spellings of
/// `ENOENT`/`EACCES`, which also cover the shebang-interpreter arm dsh
/// documents). The workdir is checked at
/// classification time, not atomically with spawn; concurrent path
/// replacement may change attribution but cannot permit an unconfined
/// execution.
#[must_use]
pub fn is_runner_spawn_failure(
    error: &io::Error,
    runner_program: Option<&str>,
    workdir: &Path,
) -> bool {
    if runner_program.is_none() || !is_usable_workdir(workdir) {
        return false;
    }
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

/// Classifies one settled process against the selected backend's structured runner-failure rules (dsh `classifyRunnerFailure`).
///
/// Each
/// rule requires a nonzero exit, its optional exit-code gate, and a
/// fatal signature on one stderr line after exact informational lines
/// are excluded.
///
/// Returns the first matching fatal line, or `None` when evidence is
/// insufficient (`exit_code` `None` means signal termination).
#[must_use]
pub fn classify_runner_failure(
    exit_code: Option<i32>,
    stderr: &str,
    rules: &[RunnerFailureRule],
) -> Option<String> {
    // The filter encodes both guards (None = signal termination; zero
    // = success) and `?` keeps the early return clippy wants.
    let exit_code = exit_code.filter(|code| *code != 0)?;
    let _ = exit_code;
    let lines: Vec<&str> = stderr
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    for rule in rules {
        if let Some(allowed) = rule.allowed_exit_codes.as_ref()
            && !allowed.contains(&exit_code)
        {
            continue;
        }
        let informational: Vec<String> = rule
            .informational_lines
            .iter()
            .flatten()
            .map(|line| line.to_lowercase())
            .collect();
        // An empty or whitespace-only substring is not meaningful runner
        // evidence. Ignore it while keeping any valid signatures beside
        // it active.
        let fatal: Vec<String> = rule
            .fatal_signatures
            .iter()
            .filter(|signature| !signature.trim().is_empty())
            .map(|signature| signature.to_lowercase())
            .collect();
        for line in &lines {
            let lowered = line.to_lowercase();
            if informational.contains(&lowered) {
                continue;
            }
            if fatal.iter().any(|signature| lowered.contains(signature)) {
                return Some((*line).to_owned());
            }
        }
    }
    None
}

/// Matches a non-zero exit against case-insensitive stderr signatures
/// (dsh `matchesSignature`).
#[must_use]
pub fn matches_signature(exit_code: Option<i32>, stderr: &str, signatures: &[String]) -> bool {
    if exit_code.is_none_or(|code| code == 0) {
        return false;
    }
    let lowered = stderr.to_lowercase();
    signatures
        .iter()
        .any(|signature| lowered.contains(&signature.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(fatal: &[&str]) -> RunnerFailureRule {
        RunnerFailureRule {
            allowed_exit_codes: None,
            fatal_signatures: fatal.iter().map(|s| (*s).to_owned()).collect(),
            informational_lines: None,
        }
    }

    #[test]
    fn zero_or_signalled_settlements_never_match() {
        assert_eq!(
            classify_runner_failure(None, "bwrap: boom", &[rule(&["bwrap: "])]),
            None
        );
        assert_eq!(
            classify_runner_failure(Some(0), "bwrap: boom", &[rule(&["bwrap: "])]),
            None
        );
        assert!(!matches_signature(
            None,
            "read-only file system",
            &["read-only file system".into()]
        ));
        assert!(!matches_signature(
            Some(0),
            "read-only file system",
            &["read-only file system".into()]
        ));
    }

    #[test]
    fn informational_lines_are_excluded_by_full_line_equality() {
        let gated = || RunnerFailureRule {
            allowed_exit_codes: Some(vec![125]),
            fatal_signatures: vec!["landlock-run: ".into()],
            informational_lines: Some(vec![
                "LANDLOCK-RUN: PARTIAL ENFORCEMENT (OLDER LANDLOCK ABI)".into(),
            ]),
        };
        // Case-insensitive on both sides: the informational line is
        // excluded in any casing, and the fatal signature matches in any
        // casing.
        assert_eq!(
            classify_runner_failure(
                Some(125),
                "landlock-run: partial enforcement (older Landlock ABI)\nlandlock-run: boom",
                &[gated()]
            ),
            Some("landlock-run: boom".to_owned())
        );
        assert_eq!(
            classify_runner_failure(
                Some(125),
                "landlock-run: partial enforcement (older Landlock ABI)",
                &[gated()]
            ),
            None
        );
    }

    #[test]
    fn exit_code_gates_and_blank_signatures() {
        let gated = || RunnerFailureRule {
            allowed_exit_codes: Some(vec![127]),
            fatal_signatures: vec!["windows-acl-run: ".into()],
            informational_lines: None,
        };
        assert_eq!(
            classify_runner_failure(Some(1), "windows-acl-run: boom", &[gated()]),
            None
        );
        assert_eq!(
            classify_runner_failure(Some(127), "windows-acl-run: boom", &[gated()]).as_deref(),
            Some("windows-acl-run: boom")
        );
        let blank = RunnerFailureRule {
            allowed_exit_codes: None,
            fatal_signatures: vec!["   ".into(), "real".into()],
            informational_lines: None,
        };
        assert_eq!(
            classify_runner_failure(Some(3), "REAL failure", &[blank]).as_deref(),
            Some("REAL failure")
        );
    }

    #[test]
    fn spawn_failures_need_a_runner_and_a_usable_cwd() {
        let workdir = std::env::temp_dir();
        let not_found = io::Error::from(io::ErrorKind::NotFound);
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(is_runner_spawn_failure(&not_found, Some("bwrap"), &workdir));
        assert!(is_runner_spawn_failure(&denied, Some("bwrap"), &workdir));
        assert!(!is_runner_spawn_failure(&not_found, None, &workdir));
        assert!(!is_runner_spawn_failure(
            &not_found,
            Some("bwrap"),
            Path::new("/definitely/not/a/dir")
        ));
        assert!(!is_runner_spawn_failure(
            &io::Error::from(io::ErrorKind::TimedOut),
            Some("bwrap"),
            &workdir
        ));
    }
}
