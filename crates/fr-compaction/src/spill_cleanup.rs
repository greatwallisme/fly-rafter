//! Startup cleanup mechanics for local spill roots.
//!
//! Best-effort one-shot deletion of expired regular files under the
//! session directories, pruning of emptied directories, and reclamation
//! of prior per-process default roots. The POSIX trust checks keep the
//! sweep from following or deleting through a planted symlink. The dsh
//! source is spill-local `cleanup.ts`.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// A backend-generated default root name: `dsh-spill-` plus the
/// 6-character suffix. Discovery matches this EXACT shape, not the
/// bare prefix, so an unrelated fixture directory is never swept.
fn is_default_root_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(crate::spill_local::DEFAULT_ROOT_PREFIX) else {
        return false;
    };
    rest.len() == 6 && rest.chars().all(|ch| ch.is_ascii_alphanumeric())
}

/// A backend-generated session directory name: `session-` plus the 12
/// lowercase hex characters derived from `sha256(sessionId)`. The
/// sweep only descends into entries of this EXACT shape.
fn is_session_dir_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("session-") else {
        return false;
    };
    rest.len() == 12
        && rest
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
}

/// A one-argument warning sink — the sweep's only side effect on
/// failure (never throws).
pub type WarnFn<'a> = &'a dyn Fn(&str);

/// Reports a best-effort sweep failure without allowing the sink to
/// reject cleanup (the dsh `warnSafely`).
fn warn_safely(warn: WarnFn<'_>, message: &str) {
    // Warning sinks are observational callbacks; cleanup must remain
    // best-effort even when a logger implementation panics.
    let sink = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| warn(message)));
    let _ = sink;
}

/// Whether another local OS user cannot replace children of this
/// directory (the dsh `isTrustedDirectory`: owned by the current user
/// and not group/other writable).
fn is_trusted_directory(metadata: &fs::Metadata) -> bool {
    if !metadata.is_dir() {
        return false;
    }
    metadata.uid() == current_uid() && (metadata.mode() & 0o022) == 0
}

/// The effective user id (POSIX; the M1 platform gate pins Linux).
fn current_uid() -> u32 {
    // SAFETY: `geteuid` is a plain POSIX id read with no preconditions.
    unsafe { libc::geteuid() }
}

/// Stable identity for de-duplicating aliases of one root (the dsh
/// `rootIdentity`: POSIX device and inode).
fn root_identity(metadata: &fs::Metadata) -> String {
    format!("{}:{}", metadata.dev(), metadata.ino())
}

/// Checks that no ancestor permits another local OS user to replace
/// the selected child (the dsh `hasProtectedAncestors`): a sticky
/// writable ancestor is safe because the child is owned by the
/// current user, which admits normal per-process roots below `/tmp`.
fn has_protected_ancestors(path: &Path) -> Result<bool, std::io::Error> {
    let current_uid = current_uid();
    let mut child = path.to_path_buf();
    let mut child_metadata = fs::symlink_metadata(&child)?;
    loop {
        let Some(parent) = child.parent() else {
            return Ok(true);
        };
        if parent == child {
            return Ok(true);
        }
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() {
            return Ok(false);
        }
        let writable_by_others = (metadata.mode() & 0o022) != 0;
        let sticky = (metadata.mode() & 0o1000) != 0;
        if writable_by_others && !sticky {
            return Ok(false);
        }
        if writable_by_others && child_metadata.uid() != current_uid {
            return Ok(false);
        }
        child = parent.to_path_buf();
        child_metadata = metadata;
    }
}

/// An existing root resolved to one stable filesystem identity (the
/// dsh `ResolvedRoot`).
struct ResolvedRoot {
    /// Canonical absolute path used for the sweep.
    path: PathBuf,
    /// Device/inode identity used to de-duplicate filesystem aliases.
    identity: String,
}

/// Resolves one existing root without admitting a directory another
/// local OS user can replace during the sweep (the dsh `resolveRoot`).
/// A configured root may be a symlink; discovery passes `false` so a
/// symlink cannot impersonate a default root.
fn resolve_root(path: &Path, allow_symlink: bool, warn: WarnFn<'_>) -> Option<ResolvedRoot> {
    let initial = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn_safely(
                    warn,
                    &format!(
                        "spill-local: failed to inspect root {}: {error}",
                        path.display()
                    ),
                );
            }
            return None;
        }
    };
    if initial.is_symlink() {
        if !allow_symlink {
            return None;
        }
    } else if !is_trusted_directory(&initial) {
        warn_safely(
            warn,
            &format!(
                "spill-local: skipped unsafe root {}: expected a directory owned by the current \
                 user and not writable by group or others",
                path.display()
            ),
        );
        return None;
    }

    let canonical = match fs::canonicalize(path) {
        Ok(canonical) => canonical,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn_safely(
                    warn,
                    &format!(
                        "spill-local: failed to resolve root {}: {error}",
                        path.display()
                    ),
                );
            }
            return None;
        }
    };
    let stats = match fs::symlink_metadata(&canonical) {
        Ok(metadata) => metadata,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn_safely(
                    warn,
                    &format!(
                        "spill-local: failed to resolve root {}: {error}",
                        path.display()
                    ),
                );
            }
            return None;
        }
    };
    let protected_ancestors = match has_protected_ancestors(&canonical) {
        Ok(protected) => protected,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn_safely(
                    warn,
                    &format!(
                        "spill-local: failed to inspect ancestors of root {}: {error}",
                        canonical.display()
                    ),
                );
            }
            return None;
        }
    };
    if !is_trusted_directory(&stats) || !protected_ancestors {
        warn_safely(
            warn,
            &format!(
                "spill-local: skipped unsafe root {}: expected a current-user-owned directory \
                 with protected write and ancestor permissions",
                canonical.display()
            ),
        );
        return None;
    }
    Some(ResolvedRoot {
        identity: root_identity(&stats),
        path: canonical,
    })
}

/// One root to sweep, plus whether the root itself may be pruned once
/// empty (the dsh `SweepRoot`; only a discovered prior-default root is
/// pruned — the active root the live process writes into never is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepRoot {
    /// Absolute spill root to sweep.
    pub path: PathBuf,
    /// Remove the root after its empty `session-*` children are pruned.
    pub prune_when_empty: bool,
}

/// Deletes one path, treating a concurrent-race disappearance as
/// success (the dsh `unlinkIdempotent`: ENOENT means the goal already
/// holds; any other error is reported and swallowed).
fn unlink_idempotent(path: &Path, warn: WarnFn<'_>) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        warn_safely(
            warn,
            &format!("spill-local: failed to delete {}: {error}", path.display()),
        );
    }
}

/// The epoch-millisecond modified time of one entry (the dsh
/// `mtimeMs` view the cutoff compares).
fn mtime_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |age| i64::try_from(age.as_millis()).unwrap_or(i64::MAX))
}

/// Sweeps one spill session directory: delete expired regular files,
/// skip everything else, and report whether the directory holds no
/// entries afterward (the dsh `sweepSessionDir`). A symlink or any
/// non-regular entry is left untouched; every per-entry failure is
/// contained.
fn sweep_session_dir(dir: &Path, cutoff_ms: i64, warn: WarnFn<'_>) -> bool {
    let names = match fs::read_dir(dir) {
        Ok(entries) => {
            let names: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            names
        }
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn_safely(
                    warn,
                    &format!("spill-local: failed to read {}: {error}", dir.display()),
                );
            }
            return false;
        }
    };
    let mut remaining = names.len();
    for path in names {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    remaining -= 1;
                    continue;
                }
                warn_safely(
                    warn,
                    &format!("spill-local: failed to stat {}: {error}", path.display()),
                );
                continue;
            }
        };
        // Only regular files expire. Symlinks and other special entries
        // are skipped (never followed) so the sweep cannot be
        // redirected or delete a link.
        if !metadata.is_file() {
            continue;
        }
        if mtime_ms(&metadata) >= cutoff_ms {
            continue;
        }
        unlink_idempotent(&path, warn);
        remaining -= 1;
    }
    remaining == 0
}

/// Best-effort one-shot cleanup across each root (the dsh
/// `sweepSpillRoots`): delete expired regular files under its
/// `session-*` directories and prune every empty session directory.
///
/// Only a discovered prior-default root is itself removed. Every
/// filesystem and warning-sink failure is contained.
pub fn sweep_spill_roots(roots: &[SweepRoot], cutoff_ms: i64, warn: WarnFn<'_>) {
    let mut resolved: HashMap<String, SweepRoot> = HashMap::new();
    for candidate in roots {
        let Some(entry) = resolve_root(&candidate.path, false, warn) else {
            continue;
        };
        let sweep = resolved.entry(entry.identity).or_insert(SweepRoot {
            path: entry.path,
            prune_when_empty: true,
        });
        sweep.prune_when_empty = sweep.prune_when_empty && candidate.prune_when_empty;
    }
    for root in resolved.into_values() {
        let entries = match fs::read_dir(&root.path) {
            Ok(entries) => entries,
            Err(error) => {
                // A root that does not exist yet (no spill ever
                // written) is the common case, not an error: ENOENT is
                // silent, anything else is reported.
                if error.kind() != std::io::ErrorKind::NotFound {
                    warn_safely(
                        warn,
                        &format!(
                            "spill-local: failed to read root {}: {error}",
                            root.path.display()
                        ),
                    );
                }
                continue;
            }
        };
        // Track whether the root holds ANY entry the sweep did not
        // fully reclaim, so a discovered prior-default root can be
        // pruned only when nothing remains.
        let mut root_emptiable = true;
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !is_session_dir_name(&name) {
                root_emptiable = false;
                continue;
            }
            let dir = entry.path();
            let metadata = match fs::symlink_metadata(&dir) {
                Ok(metadata) => metadata,
                Err(error) => {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        warn_safely(
                            warn,
                            &format!("spill-local: failed to stat {}: {error}", dir.display()),
                        );
                    }
                    continue;
                }
            };
            if !is_trusted_directory(&metadata) {
                warn_safely(
                    warn,
                    &format!(
                        "spill-local: skipped unsafe session directory {}",
                        dir.display()
                    ),
                );
                root_emptiable = false;
                continue;
            }
            if !sweep_session_dir(&dir, cutoff_ms, warn) {
                root_emptiable = false;
                continue;
            }
            if let Err(error) = fs::remove_dir(&dir)
                && error.kind() != std::io::ErrorKind::NotFound
                && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
            {
                warn_safely(
                    warn,
                    &format!("spill-local: failed to prune {}: {error}", dir.display()),
                );
            }
        }
        if root.prune_when_empty
            && root_emptiable
            && let Err(error) = fs::remove_dir(&root.path)
            && error.kind() != std::io::ErrorKind::NotFound
            && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
        {
            warn_safely(
                warn,
                &format!(
                    "spill-local: failed to prune root {}: {error}",
                    root.path.display()
                ),
            );
        }
    }
}

/// Discovers prior default spill roots (the dsh
/// `discoverDefaultRootRecords`): the `dsh-spill-<6 chars>` directories
/// directly under `base` that earlier default-root runs created.
///
/// Symlinks and non-directories are excluded. Returns canonical paths
/// of trusted roots, or an empty vector when the scan fails.
#[must_use]
pub fn discover_default_roots(base: &Path, warn: WarnFn<'_>) -> Vec<PathBuf> {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) => {
            warn_safely(
                warn,
                &format!(
                    "spill-local: failed to scan {} for default roots: {error}",
                    base.display()
                ),
            );
            return Vec::new();
        }
    };
    let mut roots = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        if !is_default_root_name(&name.to_string_lossy()) {
            continue;
        }
        if let Some(resolved) = resolve_root(&entry.path(), false, warn) {
            roots.push(resolved.path);
        }
    }
    roots
}

/// Gathers and de-duplicates the trusted roots for one startup sweep
/// (the dsh `gatherSweepRoots`).
///
/// The discovered prior-default roots sit below `default_roots_base`
/// (the OS temp dir — where per-process default roots accumulate); the
/// active root joins with its resolved identity marked non-prunable.
#[must_use]
pub fn gather_sweep_roots(
    active_root: &Path,
    default_roots_base: &Path,
    warn: WarnFn<'_>,
) -> Vec<SweepRoot> {
    let mut roots: HashMap<String, SweepRoot> = HashMap::new();
    for path in discover_default_roots(default_roots_base, warn) {
        if let Some(resolved) = resolve_root(&path, false, warn) {
            roots.insert(
                resolved.identity,
                SweepRoot {
                    path: resolved.path,
                    prune_when_empty: true,
                },
            );
        }
    }
    if let Some(active) = resolve_root(active_root, true, warn) {
        roots.insert(
            active.identity,
            SweepRoot {
                path: active.path,
                prune_when_empty: false,
            },
        );
    }
    roots.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::{SweepRoot, discover_default_roots, gather_sweep_roots, sweep_spill_roots};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fr-spill-sweep-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("root");
        // The umask here is group-write (002); the backend's own roots
        // and session directories are always created private, so the
        // fixtures assert the same shape.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("root chmod");
        dir
    }

    fn session_dir(root: &Path, hash: &str) -> std::path::PathBuf {
        let dir = root.join(format!("session-{hash}"));
        std::fs::create_dir_all(&dir).expect("session dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .expect("session chmod");
        dir
    }

    fn write_file(path: &Path, age_ms: u64) {
        std::fs::write(path, "content").expect("write");
        let stamp = SystemTime::now() - Duration::from_millis(age_ms);
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open");
        file.set_modified(stamp).expect("set mtime");
    }

    /// The sweep cutoff for a 30-day retention window (epoch millis).
    fn cutoff() -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis();
        i64::try_from(now).unwrap_or(i64::MAX) - (30 * 24 * 60 * 60 * 1000)
    }

    fn warnings() -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, impl Fn(&str)) {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink_log = std::sync::Arc::clone(&log);
        (std::sync::Arc::clone(&log), move |message: &str| {
            sink_log.lock().expect("sink lock").push(message.to_owned());
        })
    }

    /// Old regular files under a session directory expire; fresh ones
    /// survive; the emptied directory is pruned.
    #[test]
    fn expired_files_delete_and_empty_dirs_prune() {
        let root = temp_root("expire");
        let dir = session_dir(&root, "aaaaaaaaaaaa");
        write_file(&dir.join("old.txt"), 40 * 24 * 60 * 60 * 1000);
        write_file(&dir.join("fresh.txt"), 1_000);
        let (log, warn) = warnings();
        sweep_spill_roots(
            &[SweepRoot {
                path: root.clone(),
                prune_when_empty: false,
            }],
            cutoff(),
            &warn,
        );
        assert!(!dir.join("old.txt").exists(), "the expired file is deleted");
        assert!(dir.join("fresh.txt").exists(), "the fresh file survives");
        assert!(dir.exists(), "the non-empty session directory stays");
        assert!(
            log.lock().expect("log").is_empty(),
            "{:?}",
            log.lock().expect("log")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fully emptied session directory is pruned; an unrelated
    /// sibling (`session-backup`) is untouched and blocks pruning the
    /// root.
    #[test]
    fn unrelated_entries_survive_and_block_root_pruning() {
        let root = temp_root("unrelated");
        let dir = session_dir(&root, "bbbbbbbbbbbb");
        write_file(&dir.join("old.txt"), 40 * 24 * 60 * 60 * 1000);
        std::fs::write(root.join("session-backup"), "keep").expect("sibling");
        let (log, warn) = warnings();
        sweep_spill_roots(
            &[SweepRoot {
                path: root.clone(),
                prune_when_empty: true,
            }],
            cutoff(),
            &warn,
        );
        assert!(!dir.exists(), "the emptied session directory is pruned");
        assert!(root.join("session-backup").exists(), "the sibling survives");
        assert!(root.exists(), "the sibling blocks pruning the root");
        assert!(log.lock().expect("log").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A `session-*` symlink is never followed (its target survives)
    /// and blocks pruning.
    #[test]
    fn session_symlinks_are_never_followed() {
        let root = temp_root("symlink");
        let foreign = temp_root("symlink-foreign");
        std::fs::write(foreign.join("target.txt"), "keep").expect("target");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&foreign, root.join("session-cccccccccccc")).expect("link");
        let (log, warn) = warnings();
        sweep_spill_roots(
            &[SweepRoot {
                path: root.clone(),
                prune_when_empty: true,
            }],
            0,
            &warn,
        );
        assert!(
            std::fs::read_to_string(foreign.join("target.txt")).is_ok(),
            "the symlink target is never touched"
        );
        assert!(root.join("session-cccccccccccc").exists());
        assert!(root.exists(), "the link blocks pruning");
        assert!(
            !log.lock().expect("log").is_empty(),
            "the unsafe skip warns"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&foreign);
    }

    /// A group-writable session directory is skipped with the verbatim
    /// warning and blocks pruning.
    #[test]
    fn unsafe_session_dirs_are_skipped_with_the_verbatim_warning() {
        let root = temp_root("unsafe");
        let dir = session_dir(&root, "dddddddddddd");
        write_file(&dir.join("old.txt"), 40 * 24 * 60 * 60 * 1000);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o770)).expect("chmod");
        let (log, warn) = warnings();
        sweep_spill_roots(
            &[SweepRoot {
                path: root.clone(),
                prune_when_empty: false,
            }],
            0,
            &warn,
        );
        assert!(
            dir.join("old.txt").exists(),
            "the unsafe directory is skipped"
        );
        assert!(
            log.lock().expect("log").iter().any(
                |message| message.starts_with("spill-local: skipped unsafe session directory ")
            ),
            "{:?}",
            log.lock().expect("log")
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod back");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A discovered prior-default root is pruned once emptied; the
    /// active root is never pruned, and the two identities merge so a
    /// shared target never goes prunable.
    #[test]
    fn discovered_roots_prune_and_the_active_root_never_does() {
        let base = temp_root("discover-base");
        let prior = base.join("dsh-spill-abcdef");
        std::fs::create_dir(&prior).expect("prior");
        std::fs::set_permissions(&prior, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let dir = session_dir(&prior, "eeeeeeeeeeee");
        write_file(&dir.join("old.txt"), 40 * 24 * 60 * 60 * 1000);
        let active = temp_root("discover-active");

        let (log, warn) = warnings();
        let roots = gather_sweep_roots(&active, &base, &warn);
        assert!(
            roots
                .iter()
                .any(|root| root.path == prior && root.prune_when_empty),
            "the prior default root is discovered prunable: {roots:?}"
        );
        assert!(
            roots
                .iter()
                .any(|root| root.path == active && !root.prune_when_empty),
            "the active root is never prunable: {roots:?}"
        );
        sweep_spill_roots(&roots, cutoff(), &warn);
        assert!(!prior.exists(), "the emptied prior default root is pruned");
        assert!(active.exists(), "the active root always survives");
        assert!(log.lock().expect("log").is_empty());

        let discovered = discover_default_roots(&base, &warn);
        assert!(
            discovered.is_empty(),
            "the pruned root is gone: {discovered:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&active);
    }

    /// The discovery regex matches only the exact `mkdtemp` shape, so
    /// an unrelated fixture prefix never sweeps.
    #[test]
    fn discovery_matches_only_the_exact_root_shape() {
        let base = temp_root("shape");
        std::fs::create_dir(base.join("dsh-spilltooshort")).expect("too long");
        std::fs::create_dir(base.join("dsh-spill-sh")).expect("non suffix form");
        let (log, warn) = warnings();
        let discovered = discover_default_roots(&base, &warn);
        assert!(discovered.is_empty(), "{discovered:?}");
        assert!(log.lock().expect("log").is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
