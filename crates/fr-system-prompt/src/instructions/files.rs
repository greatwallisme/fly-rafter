// Rust guideline compliant 2026-10-28
//! Instruction-file discovery and bounded, seam-mediated reads (dsh
//! agent-instructions/src/files.ts, verbatim semantics).
//!
//! All filesystem access rides [`FileSeam`] (Q88-A): the production
//! [`StdFs`] follows symlinks and reads the host filesystem with no
//! freshness token (the dsh node fallback shape); tests drive an
//! in-memory seam that carries write-counter versions.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::ErrorKind;

use crate::instructions::config::{ResolvedDiscovery, relative_path, resolve_path};
use crate::instructions::digest::trimmed_instruction_digest;
use crate::instructions::render::{
    AGENTS_GLOBAL_DIRECTORY, InstructionFile, LoadedInstructionFile, RenderedAgentInstructions,
    USER_GLOBAL_DIRECTORY, USER_GLOBAL_FILE, decode_scope_key, escape_project_display_path,
    instruction_candidate_group, render_agent_instruction_set, scope_for_display_path,
};

/// One seam stat: existence of any kind, confirmed absence, or failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatOutcome {
    /// The path exists; regular files carry their metadata.
    Exists {
        /// Whether the resolved target is a regular file.
        is_file: bool,
        /// The file's byte size, when known before the read.
        size: Option<u64>,
        /// Provider freshness token, when the seam carries one.
        version: Option<String>,
    },
    /// The path is missing (or not a directory entry).
    Missing,
    /// The seam could not answer.
    Unavailable,
}

/// The filesystem seam (Q88-A): dsh splits host reads from `ctx.fs`
/// provider reads; fr injects the implementation.
pub trait FileSeam {
    /// Probes one path; a final-component symlink follows to its target
    /// (a link to a regular file reads, anything else is absent).
    fn stat(&mut self, path: &str) -> StatOutcome;

    /// Reads one file's text under the per-file byte cap; over-cap and
    /// unreadable paths return `None` (the dsh whole-file skip, never a
    /// partial read at this layer).
    fn read_text_bounded(&mut self, path: &str, max_source_bytes: u64) -> Option<String>;
}

/// The production seam over the host filesystem (dsh node fallback:
/// `stat` follows symlinks, no freshness token).
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

impl FileSeam for StdFs {
    fn stat(&mut self, path: &str) -> StatOutcome {
        match std::fs::metadata(path) {
            Ok(info) => StatOutcome::Exists {
                is_file: info.is_file(),
                size: Some(info.len()),
                version: None,
            },
            Err(error)
                if error.kind() == ErrorKind::NotFound
                    || error.kind() == ErrorKind::NotADirectory =>
            {
                StatOutcome::Missing
            }
            Err(_) => StatOutcome::Unavailable,
        }
    }

    fn read_text_bounded(&mut self, path: &str, max_source_bytes: u64) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        if u64::try_from(bytes.len()).ok()? > max_source_bytes {
            return None;
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// An in-memory seam for tests: files, directories, and a write counter
/// that stands in for the provider freshness token.
#[derive(Debug, Default, Clone)]
pub struct MemoryFs {
    files: HashMap<String, String>,
    directories: HashSet<String>,
    writes: HashMap<String, u32>,
    reads: usize,
}

impl MemoryFs {
    /// Creates an empty seam.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes one file, bumping its freshness token.
    pub fn write(&mut self, path: &str, content: &str) {
        self.files.insert(path.to_string(), content.to_string());
        let count = self.writes.entry(path.to_string()).or_insert(0);
        *count += 1;
    }

    /// Deletes one file, bumping its freshness token.
    pub fn remove(&mut self, path: &str) {
        self.files.remove(path);
        let count = self.writes.entry(path.to_string()).or_insert(0);
        *count += 1;
    }

    /// Marks one directory as existing.
    pub fn add_directory(&mut self, path: &str) {
        self.directories.insert(path.to_string());
    }

    /// The current freshness token, when the path has one.
    #[must_use]
    pub fn version_of(&self, path: &str) -> Option<String> {
        self.writes.get(path).map(|count| format!("v{count}"))
    }

    /// How many bounded reads the seam served (fast-path coverage).
    #[must_use]
    pub const fn read_count(&self) -> usize {
        self.reads
    }
}

impl FileSeam for MemoryFs {
    fn stat(&mut self, path: &str) -> StatOutcome {
        if let Some(content) = self.files.get(path) {
            return StatOutcome::Exists {
                is_file: true,
                size: Some(content.len() as u64),
                version: self.version_of(path),
            };
        }
        if self.directories.contains(path) {
            return StatOutcome::Exists {
                is_file: false,
                size: None,
                version: None,
            };
        }
        // A path under a known file (ENOTDIR) and a missing path are both
        // confirmed absence.
        StatOutcome::Missing
    }

    fn read_text_bounded(&mut self, path: &str, max_source_bytes: u64) -> Option<String> {
        let content = self.files.get(path)?;
        if content.len() as u64 > max_source_bytes {
            return None;
        }
        self.reads += 1;
        Some(content.clone())
    }
}

/// Walk upward to the first directory containing a configured root
/// marker; a marker-less tree resolves to `cwd` (dsh `findProjectRoot`).
/// A seam failure propagates, exactly like the dsh host-stat throw.
///
/// # Errors
///
/// Returns the seam failure text when a marker probe cannot answer.
pub fn find_project_root(
    cwd: &str,
    markers: &[String],
    seam: &mut dyn FileSeam,
) -> Result<String, String> {
    let start = resolve_path("/", cwd);
    let mut current = start.clone();
    loop {
        for marker in markers {
            match seam.stat(&format!("{current}/{marker}")) {
                StatOutcome::Exists { .. } => return Ok(current),
                StatOutcome::Missing => {}
                StatOutcome::Unavailable => {
                    return Err(format!("marker probe unavailable at {current}/{marker}"));
                }
            }
        }
        let parent = dirname_of(&current);
        if parent == current {
            return Ok(start);
        }
        current = parent;
    }
}

/// `path.dirname` with POSIX separators.
fn dirname_of(path: &str) -> String {
    match path.rfind('/') {
        None => ".".to_string(),
        Some(0) => "/".to_string(),
        Some(at) => path[..at].to_string(),
    }
}

/// The inclusive root-to-cwd directory chain, broadest first (dsh
/// `ancestorChain`).
#[must_use]
pub fn ancestor_chain(root: &str, cwd: &str) -> Vec<String> {
    let mut chain = Vec::new();
    let resolved_root = resolve_path("/", root);
    let mut current = resolve_path("/", cwd);
    while current != resolved_root {
        chain.push(current.clone());
        let parent = dirname_of(&current);
        if parent == current {
            break;
        }
        current = parent;
    }
    chain.push(resolved_root);
    chain.reverse();
    chain
}

/// Descendant directories crossed between a cwd and a touched file,
/// shallowest first; paths outside the root contribute nothing (dsh
/// `descendantDirsBetween`).
#[must_use]
pub fn descendant_dirs_between(root: &str, touched_path: &str) -> Vec<String> {
    let resolved_root = resolve_path("/", root);
    let target = if touched_path.starts_with('/') {
        resolve_path("/", touched_path)
    } else {
        resolve_path(&resolved_root, touched_path)
    };
    let target_dir = dirname_of(&target);
    if target_dir == resolved_root || !target_dir.starts_with(&resolved_root) {
        return Vec::new();
    }
    ancestor_chain(&resolved_root, &target_dir)
        .into_iter()
        .skip(1)
        .collect()
}

/// Convert an absolute instruction path to its project display path (dsh
/// `relativeDisplay`).
#[must_use]
pub fn relative_display(root: &str, path: &str) -> String {
    escape_project_display_path(&relative_path(root, path))
}

/// Ordered user-global roots as (scope directory, home, display label):
/// the harness home first, then the shared agents root (dsh
/// `userGlobalRoots`).
fn user_global_roots(discovery: &ResolvedDiscovery) -> Vec<(&'static str, String, String)> {
    vec![
        (
            USER_GLOBAL_DIRECTORY,
            discovery.fr_home.clone(),
            discovery.fr_home_display.clone(),
        ),
        (
            AGENTS_GLOBAL_DIRECTORY,
            discovery.agents_home.clone(),
            discovery.agents_home_display.clone(),
        ),
    ]
}

/// One discovered candidate carrying its probe metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredInstructionFile {
    absolute_path: String,
    display_path: String,
    size: Option<u64>,
    version: Option<String>,
}

/// Discover user-global and root-to-cwd instruction candidates in model
/// precedence order (dsh `discoverInstructionFiles`): both user-global
/// roots first, then every chain directory's base candidates followed by
/// its local overlay.
fn discover_instruction_files(
    discovery: &ResolvedDiscovery,
    cwd: &str,
    project_root: &str,
    seam: &mut dyn FileSeam,
) -> Vec<DiscoveredInstructionFile> {
    let mut files: Vec<DiscoveredInstructionFile> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (_, home, display) in user_global_roots(discovery) {
        let absolute_path = format!("{home}/{USER_GLOBAL_FILE}");
        if let StatOutcome::Exists {
            is_file: true,
            size,
            version,
        } = seam.stat(&absolute_path)
            && seen.insert(absolute_path.clone())
        {
            files.push(DiscoveredInstructionFile {
                absolute_path,
                display_path: format!("{display}/{USER_GLOBAL_FILE}"),
                size,
                version,
            });
        }
    }
    for dir in ancestor_chain(project_root, cwd) {
        for candidates in [
            discovery.instruction_file_candidates.as_slice(),
            discovery.local_instruction_file_candidates.as_slice(),
        ] {
            for candidate in candidates {
                let absolute_path = format!("{dir}/{candidate}");
                if let StatOutcome::Exists {
                    is_file: true,
                    size,
                    version,
                } = seam.stat(&absolute_path)
                    && seen.insert(absolute_path.clone())
                {
                    files.push(DiscoveredInstructionFile {
                        absolute_path: absolute_path.clone(),
                        display_path: relative_display(project_root, &absolute_path),
                        size,
                        version,
                    });
                }
            }
        }
    }
    files
}

/// Rendered baseline plus the read and retained file sets (dsh
/// `RenderedInstructionSet`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedInstructionSet {
    /// The bounded rendering.
    pub rendered: RenderedAgentInstructions,
    /// Successfully read candidates before dedup and budgeting.
    pub observed: Vec<LoadedInstructionFile>,
    /// Candidates dropped as in-group trimmed-content duplicates.
    pub deduped: Vec<LoadedInstructionFile>,
    /// Candidates retained by dedup and the byte budget.
    pub included: Vec<LoadedInstructionFile>,
}

/// Drop later candidates whose trimmed content duplicates an earlier
/// candidate in the same group; the earliest candidate's original bytes
/// render (dsh `dedupInstructionFilesByCandidateGroup`).
#[must_use]
pub fn dedup_instruction_files_by_candidate_group(
    files: &[LoadedInstructionFile],
) -> Vec<LoadedInstructionFile> {
    let mut kept_digests_by_group: HashMap<String, HashSet<String>> = HashMap::new();
    let mut kept = Vec::new();
    for file in files {
        let group = instruction_candidate_group(&scope_for_display_path(&file.display_path));
        let digest = trimmed_instruction_digest(&file.content);
        let digests = kept_digests_by_group.entry(group).or_default();
        if digests.contains(&digest) {
            continue;
        }
        digests.insert(digest);
        kept.push(file.clone());
    }
    kept
}

/// Discover, read, deduplicate, and render the baseline instruction
/// chain (dsh `loadBaselineInstructionSet`); disabled budgets and empty
/// non-replacement chains return `None`.
pub fn load_baseline_instruction_set(
    discovery: &ResolvedDiscovery,
    cwd: &str,
    project_root: &str,
    max_bytes: usize,
    max_source_bytes: u64,
    replace_previous_baseline: bool,
    seam: &mut dyn FileSeam,
) -> Option<RenderedInstructionSet> {
    if max_bytes == 0 {
        return None;
    }
    let discovered = discover_instruction_files(discovery, cwd, project_root, seam);
    let mut loaded: Vec<LoadedInstructionFile> = Vec::new();
    for file in &discovered {
        let over_size = file.size.is_some_and(|size| size > max_source_bytes);
        if over_size {
            continue;
        }
        if let Some(content) = seam.read_text_bounded(&file.absolute_path, max_source_bytes) {
            loaded.push(LoadedInstructionFile {
                absolute_path: file.absolute_path.clone(),
                display_path: file.display_path.clone(),
                content,
                version: file.version.clone(),
            });
        }
    }
    let kept = dedup_instruction_files_by_candidate_group(&loaded);
    if kept.is_empty() {
        if !replace_previous_baseline {
            return None;
        }
        let (rendered, included) = render_agent_instruction_set(&[], max_bytes, true);
        return Some(RenderedInstructionSet {
            rendered,
            observed: Vec::new(),
            deduped: Vec::new(),
            included,
        });
    }
    let kept_paths: HashSet<&str> = kept
        .iter()
        .map(|file| file.absolute_path.as_str())
        .collect();
    let (rendered, included) =
        render_agent_instruction_set(&kept, max_bytes, replace_previous_baseline);
    Some(RenderedInstructionSet {
        rendered,
        deduped: loaded
            .iter()
            .filter(|file| !kept_paths.contains(file.absolute_path.as_str()))
            .cloned()
            .collect(),
        observed: loaded,
        included,
    })
}

/// Resolve the file and display paths for one logical instruction
/// candidate (dsh `scopeInstructionFile`).
#[must_use]
pub fn scope_instruction_file(
    scope: &str,
    project_root: &str,
    discovery: &ResolvedDiscovery,
) -> InstructionFile {
    let (directory, candidate_name) = decode_scope_key(scope);
    if let Some((_, home, display)) = user_global_roots(discovery)
        .into_iter()
        .find(|(name, _, _)| *name == directory)
    {
        return InstructionFile {
            absolute_path: format!("{home}/{candidate_name}"),
            display_path: format!("{display}/{candidate_name}"),
        };
    }
    let dir = if directory == "." {
        resolve_path("/", project_root)
    } else {
        resolve_path("/", &format!("{project_root}/{directory}"))
    };
    let absolute_path = format!("{dir}/{candidate_name}");
    InstructionFile {
        absolute_path: absolute_path.clone(),
        display_path: relative_display(project_root, &absolute_path),
    }
}

/// The seam metadata for one probed scope candidate (dsh
/// `probeScopeInstruction`): present file metadata, confirmed absence, or
/// seam failure.
pub fn probe_scope_instruction(
    scope: &str,
    project_root: &str,
    discovery: &ResolvedDiscovery,
    seam: &mut dyn FileSeam,
) -> StatOutcome {
    let candidate = scope_instruction_file(scope, project_root, discovery);
    seam.stat(&candidate.absolute_path)
}

/// Read one already-probed scope candidate under the configured source
/// cap (dsh `readScopeInstruction`).
pub fn read_scope_instruction(
    file: &InstructionFile,
    version: Option<&str>,
    max_source_bytes: u64,
    seam: &mut dyn FileSeam,
) -> Option<LoadedInstructionFile> {
    let content = seam.read_text_bounded(&file.absolute_path, max_source_bytes)?;
    Some(LoadedInstructionFile {
        absolute_path: file.absolute_path.clone(),
        display_path: file.display_path.clone(),
        content,
        version: version.map(ToString::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::config::{InstructionConfig, resolve_discovery};

    fn discovery(home: &str) -> ResolvedDiscovery {
        let config = InstructionConfig::base_patch();
        let no_env = |_: &str| None::<String>;
        resolve_discovery(&config, &no_env, home, "/w")
    }

    fn markers() -> Vec<String> {
        vec![".git".to_string()]
    }

    #[test]
    fn project_root_walk_stops_at_the_first_marker() {
        let mut seam = MemoryFs::new();
        seam.add_directory("/w/proj");
        seam.write("/w/proj/.git", "");
        assert_eq!(
            find_project_root("/w/proj/sub", &markers(), &mut seam).unwrap(),
            "/w/proj"
        );
    }

    #[test]
    fn markerless_tree_resolves_to_cwd() {
        let mut seam = MemoryFs::new();
        seam.add_directory("/w");
        assert_eq!(
            find_project_root("/w/proj", &markers(), &mut seam).unwrap(),
            "/w/proj"
        );
    }

    #[test]
    fn ancestor_chain_lists_broadest_first() {
        assert_eq!(
            ancestor_chain("/w/proj", "/w/proj/pkg"),
            ["/w/proj".to_string(), "/w/proj/pkg".to_string()]
        );
    }

    #[test]
    fn descendant_dirs_skip_the_root_and_outside_paths() {
        assert_eq!(
            descendant_dirs_between("/w/proj", "/w/proj/pkg/inner/AGENTS.md"),
            ["/w/proj/pkg".to_string(), "/w/proj/pkg/inner".to_string()]
        );
        assert_eq!(
            descendant_dirs_between("/w/proj", "/other/AGENTS.md"),
            Vec::<String>::new()
        );
        assert_eq!(
            descendant_dirs_between("/w/proj", "/w/proj/AGENTS.md"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn discovery_orders_globals_then_chain_base_then_local() {
        let mut seam = MemoryFs::new();
        let resolved = discovery("/home/u");
        seam.write("/home/u/.fr/AGENTS.md", "fr global");
        seam.write("/w/proj/AGENTS.md", "root");
        seam.write("/w/proj/pkg/AGENTS.md", "pkg");
        seam.write("/w/proj/pkg/AGENTS.local.md", "local");
        let files = discover_instruction_files(&resolved, "/w/proj/pkg", "/w/proj", &mut seam);
        let displays: Vec<&str> = files.iter().map(|f| f.display_path.as_str()).collect();
        assert_eq!(
            displays,
            [
                "~/.fr/AGENTS.md",
                "AGENTS.md",
                "pkg/AGENTS.md",
                "pkg/AGENTS.local.md"
            ]
        );
    }

    #[test]
    fn dedup_keeps_the_earliest_candidate_in_a_group() {
        let files = vec![
            LoadedInstructionFile {
                absolute_path: "/a".to_string(),
                display_path: "AGENTS.md".to_string(),
                content: "same".to_string(),
                version: None,
            },
            LoadedInstructionFile {
                absolute_path: "/b".to_string(),
                display_path: "CLAUDE.md".to_string(),
                content: " same ".to_string(),
                version: None,
            },
        ];
        let kept = dedup_instruction_files_by_candidate_group(&files);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].absolute_path, "/a");
    }

    #[test]
    fn baseline_set_reports_observed_deduped_and_included() {
        let mut seam = MemoryFs::new();
        let resolved = discovery("/home/u");
        seam.write("/w/proj/AGENTS.md", "root guidance");
        seam.write("/w/proj/CLAUDE.md", "root guidance");
        seam.write("/w/proj/pkg/AGENTS.md", "pkg guidance");
        let set = load_baseline_instruction_set(
            &resolved,
            "/w/proj/pkg",
            "/w/proj",
            4096,
            1_048_576,
            false,
            &mut seam,
        )
        .expect("baseline");
        assert_eq!(set.observed.len(), 3);
        assert_eq!(set.deduped.len(), 1, "CLAUDE.md trims to AGENTS.md content");
        assert_eq!(set.included.len(), 2);
        assert!(set.rendered.text.contains("pkg guidance"));
    }

    #[test]
    fn empty_chain_replaces_only_when_superseding() {
        let resolved = discovery("/home/u");
        let mut seam = MemoryFs::new();
        assert!(
            load_baseline_instruction_set(
                &resolved, "/w/proj", "/w/proj", 4096, 1_048_576, false, &mut seam
            )
            .is_none(),
            "a plain empty load yields nothing"
        );
        let set = load_baseline_instruction_set(
            &resolved, "/w/proj", "/w/proj", 4096, 1_048_576, true, &mut seam,
        )
        .expect("replacement");
        assert!(
            set.rendered
                .text
                .contains(crate::prompts::EMPTY_REPLACEMENT_AGENT_INSTRUCTIONS_INTRO)
        );
    }

    #[test]
    fn disabled_budget_loads_nothing() {
        let resolved = discovery("/home/u");
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/AGENTS.md", "guide");
        assert!(
            load_baseline_instruction_set(
                &resolved, "/w/proj", "/w/proj", 0, 1_048_576, false, &mut seam
            )
            .is_none()
        );
    }

    #[test]
    fn overcap_sources_skip_whole_file() {
        let resolved = discovery("/home/u");
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/AGENTS.md", "way too long");
        assert!(
            load_baseline_instruction_set(
                &resolved, "/w/proj", "/w/proj", 4096, 4, false, &mut seam
            )
            .is_none()
        );
    }

    #[test]
    fn scope_instruction_file_resolves_both_global_roots_and_project() {
        let resolved = discovery("/home/u");
        let global = scope_instruction_file(
            &crate::instructions::render::candidate_scope_key(USER_GLOBAL_DIRECTORY, "AGENTS.md"),
            "/w/proj",
            &resolved,
        );
        assert_eq!(global.absolute_path, "/home/u/.fr/AGENTS.md");
        assert_eq!(global.display_path, "~/.fr/AGENTS.md");
        let project = scope_instruction_file(
            &crate::instructions::render::candidate_scope_key("pkg", "AGENTS.md"),
            "/w/proj",
            &resolved,
        );
        assert_eq!(project.absolute_path, "/w/proj/pkg/AGENTS.md");
        assert_eq!(project.display_path, "pkg/AGENTS.md");
    }
}
