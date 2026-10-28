// Rust guideline compliant 2026-10-29
//! Internal platform-profile builders for the local sandbox provider
//! (dsh `sandbox-local/src/profiles.ts`).

use crate::roots::writable_roots;
use crate::seam::SandboxPolicy;

/// Builds the bwrap profile arguments for one file-effect policy (dsh
/// `bwrapProfileArgs`).
///
/// The read-only bind root, private `/dev`, unshared pid and proc, and
/// the die-with-parent guard; a `workspace-write` policy adds the
/// ephemeral `/tmp` tmpfs and the workspace bind.
#[must_use]
pub fn bwrap_profile_args(policy: &SandboxPolicy) -> Vec<String> {
    let mut args = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--unshare-pid",
        "--proc",
        "/proc",
        "--die-with-parent",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if matches!(policy.mode, crate::seam::ConfinedMode::WorkspaceWrite) {
        args.push("--tmpfs".to_owned());
        args.push("/tmp".to_owned());
        args.push("--bind".to_owned());
        args.push(policy.workspace_root.clone());
        args.push(policy.workspace_root.clone());
    }
    args
}

/// Builds the Landlock launcher grants for one file-effect policy (dsh
/// `landlockProfileArgs`).
///
/// Everything under `/` readable and executable, with the required
/// sinks writable; a `workspace-write` policy additionally grants
/// `/tmp` and the workspace root. Returns
/// the launcher grant arguments before the trailing separator and
/// command argv (`--ro <path>` / `--rw <path>`, read-only roots first,
/// in caller order).
#[must_use]
pub fn landlock_profile_args(policy: &SandboxPolicy) -> Vec<String> {
    let mut read_write = vec!["/dev/null".to_owned()];
    if matches!(policy.mode, crate::seam::ConfinedMode::WorkspaceWrite) {
        read_write.push("/tmp".to_owned());
        read_write.push(policy.workspace_root.clone());
    }
    let mut args = vec!["--ro".to_owned(), "/".to_owned()];
    for root in read_write {
        args.push("--rw".to_owned());
        args.push(root);
    }
    args
}

/// Quotes one path as an SBPL string literal (dsh `sbplString`):
/// backslashes and double quotes are backslash-escaped.
fn sbpl_string(path: &str) -> String {
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for character in path.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// Builds the sandbox-exec arguments and SBPL profile for one policy (dsh `seatbeltProfileArgs`).
///
/// The writable roots come from the shared
/// [`writable_roots`] helper (canonical, deduplicated) so the Seatbelt
/// grant and the in-process fs fence can never drift apart.
#[must_use]
pub fn seatbelt_profile_args(policy: &SandboxPolicy) -> Vec<String> {
    let mut forms = vec![
        "(version 1)".to_owned(),
        "(allow default)".to_owned(),
        "(deny file-write*)".to_owned(),
        format!("(allow file-write* (literal {}))", sbpl_string("/dev/null")),
    ];
    let roots = writable_roots(
        crate::seam::SandboxMode::from(policy.mode),
        &policy.workspace_root,
    );
    if !roots.is_empty() {
        let grants = roots
            .iter()
            .map(|root| format!("(subpath {})", sbpl_string(root)))
            .collect::<Vec<_>>()
            .join(" ");
        forms.push(format!("(allow file-write* {grants})"));
    }
    vec!["-p".to_owned(), forms.join(" ")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seam::ConfinedMode;

    fn policy(mode: ConfinedMode, root: &str) -> SandboxPolicy {
        SandboxPolicy {
            mode,
            workspace_root: root.to_owned(),
            session_id: None,
        }
    }

    #[test]
    fn bwrap_profiles_pin_the_mount_shape() {
        assert_eq!(
            bwrap_profile_args(&policy(ConfinedMode::ReadOnly, "/ws")),
            [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--proc",
                "/proc",
                "--die-with-parent"
            ]
        );
        assert_eq!(
            bwrap_profile_args(&policy(ConfinedMode::WorkspaceWrite, "/ws")),
            [
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--unshare-pid",
                "--proc",
                "/proc",
                "--die-with-parent",
                "--tmpfs",
                "/tmp",
                "--bind",
                "/ws",
                "/ws"
            ]
        );
    }

    #[test]
    fn landlock_grants_pin_the_ro_rw_ladder() {
        assert_eq!(
            landlock_profile_args(&policy(ConfinedMode::ReadOnly, "/ws")),
            ["--ro", "/", "--rw", "/dev/null"]
        );
        assert_eq!(
            landlock_profile_args(&policy(ConfinedMode::WorkspaceWrite, "/ws")),
            [
                "--ro",
                "/",
                "--rw",
                "/dev/null",
                "--rw",
                "/tmp",
                "--rw",
                "/ws"
            ]
        );
    }

    #[test]
    fn seatbelt_profiles_pin_the_sbpl_forms_and_escape_paths() {
        assert_eq!(
            seatbelt_profile_args(&policy(ConfinedMode::ReadOnly, "/ws")).join(" "),
            "-p (version 1) (allow default) (deny file-write*) \
(allow file-write* (literal \"/dev/null\"))"
        );
        let profile = seatbelt_profile_args(&policy(ConfinedMode::WorkspaceWrite, "/tmp"));
        assert_eq!(profile.first().map(String::as_str), Some("-p"));
        let body = &profile[1];
        assert!(
            body.starts_with(
                "(version 1) (allow default) (deny file-write*) \
(allow file-write* (literal \"/dev/null\")) (allow file-write* "
            ),
            "the write grants follow the deny forms: {body}"
        );
        // /tmp as the workspace collapses with the platform temp grant;
        // the escaped literal form is pinned by shape.
        assert!(
            body.contains("(subpath \"/tmp\")"),
            "canonical root granted: {body}"
        );
    }
}
