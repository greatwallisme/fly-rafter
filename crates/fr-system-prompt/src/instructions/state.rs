// Rust guideline compliant 2026-10-28
//! Session-visible workspace instruction state and dynamic reconciliation
//! (dsh agent-instructions/src/state.ts, verbatim semantics).
//!
//! Reconciliation compares the visible change surface with seam-visible
//! files, probes scope candidates in deduplication groups, rolls a whole
//! group back on any seam failure, renders transitions within the byte
//! budget, and returns only the transitions the rendering actually
//! represents; version-cache updates ride alongside.

use std::collections::HashSet;

use indexmap::{IndexMap, IndexSet};

use crate::instructions::config::ResolvedDiscovery;
use crate::instructions::digest::{instruction_content_sha1, trimmed_instruction_digest};
use crate::instructions::files::{
    FileSeam, StatOutcome, descendant_dirs_between, probe_scope_instruction,
    read_scope_instruction, relative_display, scope_instruction_file,
};
use crate::instructions::render::{
    AgentInstructionChange, ChangeRenderItem, InstructionChangeAction, InstructionFile,
    LoadedInstructionFile, USER_GLOBAL_DIRECTORIES, USER_GLOBAL_FILE, candidate_scope_key,
    decode_scope_key, instruction_candidate_group, is_user_global_directory,
    render_instruction_changes,
};

/// Per-scope metadata cache; instruction prose is deliberately not
/// retained (dsh `InstructionVersionState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionVersionState {
    /// The model-facing path.
    pub path: String,
    /// Provider freshness token, when the read carried one.
    pub version: Option<String>,
    /// The exact-content digest.
    pub digest: String,
    /// The trimmed-content identity for fast-path duplicate checks.
    pub trimmed_digest: String,
}

/// Session-isolated fast-path state keyed by logical instruction scope.
pub type VersionCache = IndexMap<String, InstructionVersionState>;

/// One session-visible message reduced to its instruction-relevant
/// faces (the loop bridge extracts this from user/message events).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VisibleMessage {
    /// The message's `source` object.
    pub source: serde_json::Map<String, serde_json::Value>,
    /// The message's `content` array, for same-payload comparisons.
    pub content: serde_json::Value,
}

/// A metadata-cache transition associated with one rendered instruction
/// change; `state: None` removes the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionVersionUpdate {
    /// The transition the update rides with.
    pub change: AgentInstructionChange,
    /// The retained metadata, when the scope stays cached.
    pub state: Option<InstructionVersionState>,
}

/// Rendered reconciliation plus its metadata-cache transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// The rendered instruction text.
    pub text: String,
    /// The transitions the rendering represents.
    pub changes: Vec<AgentInstructionChange>,
    /// The metadata-cache transitions to commit.
    pub version_updates: Vec<InstructionVersionUpdate>,
}

/// Parse one message source's workspace changes, tolerating unknown
/// shapes (dsh `workspaceInstructionChanges`).
#[must_use]
pub fn workspace_instruction_changes(
    source: &serde_json::Map<String, serde_json::Value>,
) -> Vec<AgentInstructionChange> {
    let mut changes = Vec::new();
    let Some(entries) = source.get("changes").and_then(|value| value.as_array()) else {
        return changes;
    };
    for value in entries {
        let Some(record) = value.as_object() else {
            continue;
        };
        let action = match record.get("action").and_then(|value| value.as_str()) {
            Some("set") => InstructionChangeAction::Set,
            Some("replace") => InstructionChangeAction::Replace,
            Some("remove") => InstructionChangeAction::Remove,
            _ => continue,
        };
        let (Some(scope), Some(path)) = (
            record.get("scope").and_then(|value| value.as_str()),
            record.get("path").and_then(|value| value.as_str()),
        ) else {
            continue;
        };
        let digest = match record.get("digest") {
            None => None,
            Some(value) => match value.as_str() {
                Some(text) => Some(text.to_string()),
                None => continue,
            },
        };
        changes.push(AgentInstructionChange {
            action,
            scope: scope.to_string(),
            path: path.to_string(),
            digest,
        });
    }
    changes
}

/// Whether one message source is an agent-instructions source.
fn is_agent_instructions_source(source: &serde_json::Map<String, serde_json::Value>) -> bool {
    source.get("kind").and_then(|value| value.as_str()) == Some("agent-instructions")
        && source
            .get("changes")
            .and_then(|value| value.as_array())
            .is_some()
}

/// Fold the session surface, then let authority messages override
/// scope-by-scope (dsh `visibleInstructionChanges`): later writes win.
#[must_use]
pub fn visible_instruction_changes(
    surface: &[VisibleMessage],
    authority: &[VisibleMessage],
) -> IndexMap<String, AgentInstructionChange> {
    let mut visible = IndexMap::new();
    for messages in [surface, authority] {
        for message in messages {
            if !is_agent_instructions_source(&message.source) {
                continue;
            }
            for change in workspace_instruction_changes(&message.source) {
                visible.insert(change.scope.clone(), change);
            }
        }
    }
    visible
}

/// Convert retained baseline files into comparison and metadata-cache
/// state (dsh `baselineInstructionState`).
#[must_use]
pub fn baseline_instruction_state(
    files: &[LoadedInstructionFile],
) -> (
    IndexMap<String, AgentInstructionChange>,
    IndexMap<String, InstructionVersionState>,
) {
    let mut changes = IndexMap::new();
    let mut versions = IndexMap::new();
    for file in files {
        let digest = instruction_content_sha1(&file.content);
        let scope = crate::instructions::render::instruction_scope_key(&file.display_path);
        let change = AgentInstructionChange {
            action: InstructionChangeAction::Set,
            scope: scope.clone(),
            path: file.display_path.clone(),
            digest: Some(digest.clone()),
        };
        changes.insert(scope.clone(), change);
        if let Some(version) = &file.version {
            versions.insert(
                scope,
                InstructionVersionState {
                    path: file.display_path.clone(),
                    version: Some(version.clone()),
                    digest,
                    trimmed_digest: trimmed_instruction_digest(&file.content),
                },
            );
        }
    }
    (changes, versions)
}

/// Keep only cache updates represented by rendered changes (dsh
/// `retainedInstructionVersionUpdates`).
#[must_use]
pub fn retained_instruction_version_updates(
    updates: &[InstructionVersionUpdate],
    rendered_changes: &[AgentInstructionChange],
) -> Vec<InstructionVersionUpdate> {
    updates
        .iter()
        .filter(|update| {
            rendered_changes.iter().any(|change| {
                change.action == update.change.action
                    && change.scope == update.change.scope
                    && change.path == update.change.path
                    && change.digest == update.change.digest
            })
        })
        .cloned()
        .collect()
}

/// Apply metadata-cache transitions (dsh
/// `applyInstructionVersionUpdates`); an empty batch is a no-op.
pub fn apply_instruction_version_updates(
    versions: &mut VersionCache,
    updates: &[InstructionVersionUpdate],
) {
    if updates.is_empty() {
        return;
    }
    for update in updates {
        match &update.state {
            None => {
                versions.shift_remove(&update.change.scope);
            }
            Some(state) => {
                versions.insert(update.change.scope.clone(), state.clone());
            }
        }
    }
}

/// The reconciliation inputs (dsh `reconcileInstructionContext` options).
#[derive(Debug)]
pub struct ReconcileInput<'a> {
    /// Normalized discovery configuration.
    pub discovery: &'a ResolvedDiscovery,
    /// Render byte budget.
    pub max_bytes: usize,
    /// Per-file source cap.
    pub max_source_bytes: u64,
    /// The session working directory.
    pub cwd: &'a str,
    /// The resolved project root.
    pub project_root: &'a str,
    /// The session surface's agent-instructions sources.
    pub surface: &'a [VisibleMessage],
    /// The claimed batch's agent-instructions sources.
    pub authority: &'a [VisibleMessage],
    /// The pending projection's sources.
    pub scope_messages: &'a [VisibleMessage],
    /// Paths the fs tools touched since the last reconciliation.
    pub touched_paths: &'a [String],
    /// Whether baseline scopes participate.
    pub include_baseline_scopes: bool,
    /// Baseline scopes excluded by preparation, when one applies.
    pub excluded_baseline_scopes: Option<&'a HashSet<String>>,
}

/// The project-relative scope of one directory ('.' at the root).
fn relative_scope(project_root: &str, dir: &str) -> String {
    let scope = relative_display(project_root, dir);
    if scope.is_empty() {
        ".".to_string()
    } else {
        scope
    }
}

/// Add every candidate key of one directory scope to a target set.
fn add_dir_scopes(target: &mut IndexSet<String>, discovery: &ResolvedDiscovery, directory: &str) {
    for candidate in &discovery.instruction_file_candidates {
        target.insert(candidate_scope_key(directory, candidate));
    }
    for candidate in &discovery.local_instruction_file_candidates {
        target.insert(candidate_scope_key(directory, candidate));
    }
}

/// Assemble the reconciliation scope set and its baseline subset (dsh
/// `reconcileInstructionContext` scope collection: baseline scopes, scope
/// messages, visible effective state, and touched descendant paths).
fn collect_reconcile_scopes(
    input: &ReconcileInput<'_>,
    effective: &IndexMap<String, AgentInstructionChange>,
    discovery: &ResolvedDiscovery,
) -> (IndexSet<String>, IndexSet<String>) {
    let mut scopes: IndexSet<String> = IndexSet::new();
    let mut baseline_scopes: IndexSet<String> = IndexSet::new();
    for directory in USER_GLOBAL_DIRECTORIES {
        baseline_scopes.insert(candidate_scope_key(directory, USER_GLOBAL_FILE));
    }
    for dir in crate::instructions::files::ancestor_chain(input.project_root, input.cwd) {
        let scope = relative_scope(input.project_root, &dir);
        for candidate in &discovery.instruction_file_candidates {
            baseline_scopes.insert(candidate_scope_key(&scope, candidate));
        }
        for candidate in &discovery.local_instruction_file_candidates {
            baseline_scopes.insert(candidate_scope_key(&scope, candidate));
        }
    }
    if input.include_baseline_scopes {
        scopes.extend(baseline_scopes.iter().cloned());
    }
    for message in input.scope_messages {
        if !is_agent_instructions_source(&message.source) {
            continue;
        }
        for change in workspace_instruction_changes(&message.source) {
            if !input.include_baseline_scopes && baseline_scopes.contains(&change.scope) {
                continue;
            }
            scopes.insert(change.scope);
        }
    }
    for scope in effective.keys() {
        if !input.include_baseline_scopes && baseline_scopes.contains(scope) {
            continue;
        }
        let (directory, _) = decode_scope_key(scope);
        if is_user_global_directory(&directory) {
            scopes.insert(candidate_scope_key(&directory, USER_GLOBAL_FILE));
        } else {
            add_dir_scopes(&mut scopes, discovery, &directory);
        }
    }
    for touched in input.touched_paths {
        for dir in descendant_dirs_between(input.cwd, touched) {
            let scope = relative_scope(input.project_root, &dir);
            for candidate in &discovery.instruction_file_candidates {
                scopes.insert(candidate_scope_key(&scope, candidate));
            }
            for candidate in &discovery.local_instruction_file_candidates {
                scopes.insert(candidate_scope_key(&scope, candidate));
            }
        }
    }
    (scopes, baseline_scopes)
}

/// The mutable accumulators one reconciliation pass shares across scope
/// groups.
#[derive(Default)]
struct ReconcilePass {
    /// Absolute paths already probed this pass (project aliases must not
    /// publish content withheld by a failed group).
    seen_absolute_paths: HashSet<String>,
    /// Per-group trimmed identities kept so far, in candidate order.
    kept_trimmed_by_group: IndexMap<String, IndexSet<String>>,
    /// Render items accumulated across groups.
    items: Vec<ChangeRenderItem>,
    /// Version-cache transitions accumulated across groups.
    version_updates: Vec<InstructionVersionUpdate>,
}

/// One scope's present-file view handed to the processing step.
struct ScopeView<'a> {
    /// The scope key.
    scope: &'a str,
    /// The scope's candidate group key.
    group_key: &'a str,
    /// The resolved candidate paths.
    candidate: InstructionFile,
    /// The probe's freshness token.
    version: Option<String>,
    /// The visible change for this scope, when one exists.
    previous: Option<&'a AgentInstructionChange>,
}

/// The metadata fast path's verdict.
enum FastPath {
    /// Unchanged and previously rendered; kept (or removed as a later
    /// duplicate).
    HitKept,
    /// Hidden duplicate of already-kept trimmed content; skip without a
    /// read.
    HitDuplicate,
}

/// Append one removal transition (dsh `pushRemoval`).
fn push_removal(
    items: &mut Vec<ChangeRenderItem>,
    updates: &mut Vec<InstructionVersionUpdate>,
    scope: &str,
    path: &str,
    state: Option<InstructionVersionState>,
) {
    let change = AgentInstructionChange {
        action: InstructionChangeAction::Remove,
        scope: scope.to_string(),
        path: path.to_string(),
        digest: None,
    };
    items.push(ChangeRenderItem {
        change: change.clone(),
        file: LoadedInstructionFile {
            absolute_path: format!("removed:{scope}"),
            display_path: path.to_string(),
            content: String::new(),
            version: None,
        },
    });
    updates.push(InstructionVersionUpdate { change, state });
}

/// Probe every scope group in deduplication order (dsh
/// `reconcileInstructionContext` group loop): excluded baseline scopes
/// remove, seam failures roll the whole group back, and present files
/// continue into [`process_present_scope`].
fn reconcile_groups(
    input: &ReconcileInput<'_>,
    effective: &IndexMap<String, AgentInstructionChange>,
    scopes: &IndexSet<String>,
    baseline_scopes: &IndexSet<String>,
    versions: &mut VersionCache,
    seam: &mut dyn FileSeam,
    pass: &mut ReconcilePass,
) {
    let mut scopes_by_group: IndexMap<String, Vec<String>> = IndexMap::new();
    for scope in scopes {
        let (directory, _) = decode_scope_key(scope);
        let group = instruction_candidate_group(&directory);
        scopes_by_group
            .entry(group)
            .or_default()
            .push(scope.clone());
    }
    for (group, group_scopes) in scopes_by_group {
        let mut probed_scopes: Vec<String> = Vec::new();
        for scope in &group_scopes {
            if let Some(excluded) = input.excluded_baseline_scopes
                && baseline_scopes.contains(scope)
                && excluded.contains(scope)
            {
                match effective.get(scope) {
                    None => {
                        versions.shift_remove(scope);
                    }
                    Some(previous) if previous.action == InstructionChangeAction::Remove => {
                        versions.shift_remove(scope);
                    }
                    Some(previous) => push_removal(
                        &mut pass.items,
                        &mut pass.version_updates,
                        scope,
                        &previous.path,
                        None,
                    ),
                }
                continue;
            }
            probed_scopes.push(scope.clone());
        }
        probe_group_scopes(
            input,
            effective,
            &group,
            &probed_scopes,
            versions,
            seam,
            pass,
        );
    }
}

/// Probe one group's scopes (dsh group probe loop): seam failures and
/// read failures roll the whole group back through `rollback_group`.
fn probe_group_scopes(
    input: &ReconcileInput<'_>,
    effective: &IndexMap<String, AgentInstructionChange>,
    group: &str,
    probed_scopes: &[String],
    versions: &mut VersionCache,
    seam: &mut dyn FileSeam,
    pass: &mut ReconcilePass,
) {
    let discovery = input.discovery;
    let item_start = pass.items.len();
    let update_start = pass.version_updates.len();
    let prior_versions: Vec<(String, Option<InstructionVersionState>)> = probed_scopes
        .iter()
        .map(|scope| (scope.clone(), versions.get(scope).cloned()))
        .collect();
    for scope in probed_scopes {
        let previous = effective.get(scope);
        let probe = probe_scope_instruction(scope, input.project_root, discovery, seam);
        let version = match probe {
            StatOutcome::Unavailable => {
                let seen = rollback_group(
                    &mut pass.items,
                    &mut pass.version_updates,
                    versions,
                    &mut pass.kept_trimmed_by_group,
                    &prior_versions,
                    probed_scopes,
                    input.project_root,
                    discovery,
                    group,
                    item_start,
                    update_start,
                );
                pass.seen_absolute_paths.extend(seen);
                break;
            }
            StatOutcome::Missing => {
                handle_missing_scope(versions, pass, scope, previous);
                continue;
            }
            StatOutcome::Exists {
                is_file: true,
                version,
                ..
            } => version,
            StatOutcome::Exists { is_file: false, .. } => continue,
        };
        let candidate = scope_instruction_file(scope, input.project_root, discovery);
        if pass.seen_absolute_paths.contains(&candidate.absolute_path) {
            continue;
        }
        pass.seen_absolute_paths
            .insert(candidate.absolute_path.clone());
        let view = ScopeView {
            scope,
            group_key: group,
            candidate,
            version,
            previous,
        };
        if process_present_scope(input, &view, versions, seam, pass) == ScopeFlow::Abort {
            let seen = rollback_group(
                &mut pass.items,
                &mut pass.version_updates,
                versions,
                &mut pass.kept_trimmed_by_group,
                &prior_versions,
                probed_scopes,
                input.project_root,
                discovery,
                group,
                item_start,
                update_start,
            );
            pass.seen_absolute_paths.extend(seen);
            break;
        }
    }
}

/// Handle one confirmed-absent scope (dsh absent branch): uncached or
/// already-removed scopes drop their cache entry, visible ones remove.
fn handle_missing_scope(
    versions: &mut VersionCache,
    pass: &mut ReconcilePass,
    scope: &str,
    previous: Option<&AgentInstructionChange>,
) {
    match previous {
        None => {
            versions.shift_remove(scope);
        }
        Some(previous) if previous.action == InstructionChangeAction::Remove => {
            versions.shift_remove(scope);
        }
        Some(previous) => push_removal(
            &mut pass.items,
            &mut pass.version_updates,
            scope,
            &previous.path,
            None,
        ),
    }
}

/// One processed scope's flow control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeFlow {
    /// Continue with the next scope in the group.
    Continue,
    /// The read failed; roll the group back.
    Abort,
}

/// Process one seam-present scope (dsh
/// `reconcileInstructionContext` present branch): the metadata fast
/// path, the bounded read, trimmed-duplicate collapsing, and the
/// set/replace transitions.
fn process_present_scope(
    input: &ReconcileInput<'_>,
    view: &ScopeView<'_>,
    versions: &mut VersionCache,
    seam: &mut dyn FileSeam,
    pass: &mut ReconcilePass,
) -> ScopeFlow {
    let cached = versions.get(view.scope).cloned();
    let metadata_unchanged = cached.as_ref().is_some_and(|state| {
        state.path == view.candidate.display_path && state.version == view.version
    });
    if metadata_unchanged
        && let Some(cached) = cached.as_ref()
        && fast_path(pass, view.previous, view.scope, view.group_key, cached).is_some()
    {
        return ScopeFlow::Continue;
    }
    let Some(file) = read_scope_instruction(
        &view.candidate,
        view.version.as_deref(),
        input.max_source_bytes,
        seam,
    ) else {
        return ScopeFlow::Abort;
    };
    let current_digest = instruction_content_sha1(&file.content);
    let trimmed_digest = trimmed_instruction_digest(&file.content);
    let next_version = InstructionVersionState {
        path: file.display_path.clone(),
        version: view.version.clone(),
        digest: current_digest.clone(),
        trimmed_digest: trimmed_digest.clone(),
    };
    // IndexSet::insert returns true for a fresh insertion, so a duplicate
    // is the negation (dsh registerKeptTrimmed returns "already kept").
    let duplicate = !pass
        .kept_trimmed_by_group
        .entry(view.group_key.to_string())
        .or_default()
        .insert(trimmed_digest);
    if duplicate {
        // A distinct file whose trimmed content already appeared earlier
        // in this group: drop it, remove any rendered copy, and keep its
        // metadata for the next pass.
        let previously_rendered = view
            .previous
            .is_some_and(|previous| previous.action != InstructionChangeAction::Remove);
        if previously_rendered && let Some(previous) = view.previous {
            push_removal(
                &mut pass.items,
                &mut pass.version_updates,
                view.scope,
                &previous.path,
                Some(next_version),
            );
        } else {
            versions.insert(view.scope.to_string(), next_version);
        }
        return ScopeFlow::Continue;
    }
    let unchanged = view.previous.is_some_and(|previous| {
        previous.action != InstructionChangeAction::Remove
            && previous.path == file.display_path
            && previous.digest == Some(current_digest.clone())
    });
    if unchanged {
        versions.insert(view.scope.to_string(), next_version);
        return ScopeFlow::Continue;
    }
    let action = view
        .previous
        .map_or(InstructionChangeAction::Set, |previous| {
            if previous.action == InstructionChangeAction::Remove {
                InstructionChangeAction::Set
            } else {
                InstructionChangeAction::Replace
            }
        });
    let change = AgentInstructionChange {
        action,
        scope: view.scope.to_string(),
        path: file.display_path.clone(),
        digest: Some(current_digest),
    };
    pass.items.push(ChangeRenderItem {
        change: change.clone(),
        file,
    });
    pass.version_updates.push(InstructionVersionUpdate {
        change,
        state: Some(next_version),
    });
    ScopeFlow::Continue
}

/// The metadata fast path (dsh `metadataUnchanged` branch): an unchanged,
/// previously rendered scope skips the read (kept, or removed when an
/// earlier group member now matches its trimmed content), and a hidden
/// duplicate skips without a read.
///
/// Returns `None` on a miss (read the file), or the hit verdict for the
/// caller to resolve without reading.
fn fast_path(
    pass: &mut ReconcilePass,
    previous: Option<&AgentInstructionChange>,
    scope: &str,
    group_key: &str,
    cached: &InstructionVersionState,
) -> Option<FastPath> {
    let rendered = previous.is_some_and(|previous| {
        previous.action != InstructionChangeAction::Remove
            && previous.path == cached.path
            && previous.digest == Some(cached.digest.clone())
    });
    if rendered {
        let duplicate_of_kept = !pass
            .kept_trimmed_by_group
            .entry(group_key.to_string())
            .or_default()
            .insert(cached.trimmed_digest.clone());
        // An earlier group member that now matches this trimmed content
        // makes this the duplicate to remove.
        if duplicate_of_kept && let Some(previous) = previous {
            push_removal(
                &mut pass.items,
                &mut pass.version_updates,
                scope,
                &previous.path,
                Some(cached.clone()),
            );
        }
        return Some(FastPath::HitKept);
    }
    let already_kept = pass
        .kept_trimmed_by_group
        .get(group_key)
        .is_some_and(|digests| digests.contains(&cached.trimmed_digest));
    let not_rendered = previous.is_none_or(|p| p.action == InstructionChangeAction::Remove);
    if not_rendered && already_kept {
        return Some(FastPath::HitDuplicate);
    }
    None
}

/// Undo one group's partial progress: truncate its items and updates,
/// restore the prior version-cache state, remember every probed path so
/// project aliases cannot publish withheld content, and drop the group's
/// kept-trimmed identities (dsh `rollbackGroup`).
#[expect(
    clippy::too_many_arguments,
    reason = "the rollback mirror carries the whole group context (dsh rollbackGroup)"
)]
fn rollback_group(
    items: &mut Vec<ChangeRenderItem>,
    updates: &mut Vec<InstructionVersionUpdate>,
    versions: &mut VersionCache,
    kept_trimmed_by_group: &mut IndexMap<String, IndexSet<String>>,
    prior_versions: &[(String, Option<InstructionVersionState>)],
    probed_scopes: &[String],
    project_root: &str,
    discovery: &ResolvedDiscovery,
    group: &str,
    item_start: usize,
    update_start: usize,
) -> HashSet<String> {
    items.truncate(item_start);
    updates.truncate(update_start);
    for (scope, prior) in prior_versions {
        match prior {
            None => {
                versions.shift_remove(scope);
            }
            Some(state) => {
                versions.insert(scope.clone(), state.clone());
            }
        }
    }
    let mut probed = HashSet::new();
    for scope in probed_scopes {
        probed.insert(scope_instruction_file(scope, project_root, discovery).absolute_path);
    }
    kept_trimmed_by_group.shift_remove(group);
    probed
}

/// Compare visible state with seam-visible files and render transitions
/// (dsh `reconcileInstructionContext`).
///
/// Scope groups roll back on any seam failure, trimmed duplicates
/// collapse, and only rendered transitions return with their cache
/// updates.
///
/// Returns `None` when nothing changed, nothing survived rendering, or
/// the seam failed for a whole group.
pub fn reconcile_instruction_context(
    input: &ReconcileInput<'_>,
    versions: &mut VersionCache,
    seam: &mut dyn FileSeam,
) -> Option<ReconcileOutcome> {
    let effective = visible_instruction_changes(input.surface, input.authority);
    let discovery = input.discovery;
    let (scopes, baseline_scopes) = collect_reconcile_scopes(input, &effective, discovery);

    let mut pass = ReconcilePass::default();
    reconcile_groups(
        input,
        &effective,
        &scopes,
        &baseline_scopes,
        versions,
        seam,
        &mut pass,
    );
    if pass.items.is_empty() {
        return None;
    }
    let (text, rendered_changes) = render_instruction_changes(&pass.items, input.max_bytes);
    // When no transition survived rendering (tiny budgets render
    // notice-only text), emit nothing and commit nothing — the uncommitted
    // versions make the next pass retry instead of spamming notice-only
    // contexts.
    if text.is_empty() || rendered_changes.is_empty() {
        return None;
    }
    Some(ReconcileOutcome {
        text,
        version_updates: retained_instruction_version_updates(
            &pass.version_updates,
            &rendered_changes,
        ),
        changes: rendered_changes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::config::{InstructionConfig, resolve_discovery};
    use crate::instructions::files::MemoryFs;

    fn discovery() -> ResolvedDiscovery {
        let config = InstructionConfig::base_patch();
        let no_env = |_: &str| None::<String>;
        resolve_discovery(&config, &no_env, "/home/u", "/w/proj")
    }

    fn source_with(changes: &[AgentInstructionChange]) -> VisibleMessage {
        let mut source = serde_json::Map::new();
        source.insert("kind".to_string(), "agent-instructions".into());
        let changes: Vec<serde_json::Value> = changes
            .iter()
            .map(|change| serde_json::to_value(change).expect("change json"))
            .collect();
        source.insert("changes".to_string(), changes.into());
        VisibleMessage {
            source,
            content: serde_json::json!([]),
        }
    }

    fn change(
        action: InstructionChangeAction,
        scope: &str,
        path: &str,
        digest: Option<&str>,
    ) -> AgentInstructionChange {
        AgentInstructionChange {
            action,
            scope: scope.to_string(),
            path: path.to_string(),
            digest: digest.map(ToString::to_string),
        }
    }

    fn input<'a>(
        discovery: &'a ResolvedDiscovery,
        surface: &'a [VisibleMessage],
        touched: &'a [String],
    ) -> ReconcileInput<'a> {
        baseline_input(discovery, surface, touched, false)
    }

    /// The baseline-visible variant: baseline scopes participate, as they
    /// do once a rendered baseline is on the surface.
    fn baseline_input<'a>(
        discovery: &'a ResolvedDiscovery,
        surface: &'a [VisibleMessage],
        touched: &'a [String],
        include_baseline: bool,
    ) -> ReconcileInput<'a> {
        ReconcileInput {
            discovery,
            max_bytes: 4096,
            max_source_bytes: 1_048_576,
            cwd: "/w/proj",
            project_root: "/w/proj",
            surface,
            authority: &[],
            scope_messages: &[],
            touched_paths: touched,
            include_baseline_scopes: include_baseline,
            excluded_baseline_scopes: None,
        }
    }

    const ROOT_SCOPE: &str = ".\u{0}AGENTS.md";
    const PKG_SCOPE: &str = "pkg\u{0}AGENTS.md";

    #[test]
    fn new_files_set_and_changed_files_replace() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/AGENTS.md", "first");
        let surface = [];
        let outcome = reconcile_instruction_context(
            &baseline_input(&resolved, &surface, &[], true),
            &mut VersionCache::new(),
            &mut seam,
        )
        .expect("first pass");
        assert_eq!(outcome.changes.len(), 1);
        assert_eq!(outcome.changes[0].action, InstructionChangeAction::Set);
        assert!(
            outcome
                .text
                .contains("Additional instructions from: AGENTS.md")
        );

        // The rendered change is now visible; a rewrite replaces it.
        let visible = [source_with(&[change(
            InstructionChangeAction::Set,
            ROOT_SCOPE,
            "AGENTS.md",
            Some("old"),
        )])];
        let mut versions = VersionCache::new();
        apply_instruction_version_updates(&mut versions, &outcome.version_updates);
        seam.write("/w/proj/AGENTS.md", "second");
        let outcome = reconcile_instruction_context(
            &baseline_input(&resolved, &visible, &[], true),
            &mut versions,
            &mut seam,
        )
        .expect("second pass");
        assert_eq!(outcome.changes[0].action, InstructionChangeAction::Replace);
        assert!(
            outcome
                .text
                .contains("Updated instructions from: AGENTS.md")
        );
        assert!(outcome.text.contains("second"));
    }

    #[test]
    fn deleted_files_remove() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        let visible = [source_with(&[change(
            InstructionChangeAction::Set,
            PKG_SCOPE,
            "pkg/AGENTS.md",
            Some("d"),
        )])];
        let outcome = reconcile_instruction_context(
            &input(&resolved, &visible, &[]),
            &mut VersionCache::new(),
            &mut seam,
        )
        .expect("removal");
        assert_eq!(outcome.changes[0].action, InstructionChangeAction::Remove);
        assert!(outcome.text.contains("Instructions removed: pkg/AGENTS.md"));
    }

    #[test]
    fn unchanged_content_emits_nothing() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/pkg/AGENTS.md", "stable");
        let digest = instruction_content_sha1("stable");
        let visible = [source_with(&[change(
            InstructionChangeAction::Set,
            PKG_SCOPE,
            "pkg/AGENTS.md",
            Some(&digest),
        )])];
        let outcome = reconcile_instruction_context(
            &input(&resolved, &visible, &[]),
            &mut VersionCache::new(),
            &mut seam,
        );
        assert!(
            outcome.is_none(),
            "digest-equal visible state is not a change"
        );
    }

    #[test]
    fn metadata_fast_path_skips_the_read() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/pkg/AGENTS.md", "stable");
        let digest = instruction_content_sha1("stable");
        let visible = [source_with(&[change(
            InstructionChangeAction::Set,
            PKG_SCOPE,
            "pkg/AGENTS.md",
            Some(&digest),
        )])];
        // Seed the cache with the file's exact metadata.
        let mut versions = VersionCache::new();
        versions.insert(
            PKG_SCOPE.to_string(),
            InstructionVersionState {
                path: "pkg/AGENTS.md".to_string(),
                version: seam.version_of("/w/proj/pkg/AGENTS.md"),
                digest,
                trimmed_digest: trimmed_instruction_digest("stable"),
            },
        );
        assert_eq!(seam.read_count(), 0);
        let outcome = reconcile_instruction_context(
            &input(&resolved, &visible, &[]),
            &mut versions,
            &mut seam,
        );
        assert!(outcome.is_none());
        assert_eq!(seam.read_count(), 0, "the fast path never re-reads");
    }

    #[test]
    fn trimmed_duplicates_remove_the_later_render() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/AGENTS.md", "shared");
        seam.write("/w/proj/CLAUDE.md", "shared");
        let ag_scope = candidate_scope_key(".", "AGENTS.md");
        let claude_scope = candidate_scope_key(".", "CLAUDE.md");
        let visible = [source_with(&[
            change(
                InstructionChangeAction::Set,
                &ag_scope,
                "AGENTS.md",
                Some("x"),
            ),
            change(
                InstructionChangeAction::Set,
                &claude_scope,
                "CLAUDE.md",
                Some("x"),
            ),
        ])];
        let outcome = reconcile_instruction_context(
            &baseline_input(&resolved, &visible, &[], true),
            &mut VersionCache::new(),
            &mut seam,
        )
        .expect("duplicate pass");
        let actions: Vec<InstructionChangeAction> =
            outcome.changes.iter().map(|c| c.action).collect();
        assert_eq!(actions.len(), 2);
        assert!(actions.contains(&InstructionChangeAction::Remove));
        assert!(
            outcome
                .changes
                .iter()
                .any(|c| c.path == "CLAUDE.md" && c.action == InstructionChangeAction::Remove)
        );
    }

    #[test]
    fn touched_paths_add_descendant_scopes() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/pkg/AGENTS.md", "nested");
        let touched = vec!["/w/proj/pkg/src/main.rs".to_string()];
        let outcome = reconcile_instruction_context(
            &input(&resolved, &[], &touched),
            &mut VersionCache::new(),
            &mut seam,
        )
        .expect("touched pass");
        assert_eq!(outcome.changes[0].path, "pkg/AGENTS.md");
    }

    #[test]
    fn tiny_budgets_emit_nothing_and_commit_nothing() {
        let resolved = discovery();
        let mut seam = MemoryFs::new();
        seam.write("/w/proj/pkg/AGENTS.md", "content that cannot fit");
        let mut small = input(&resolved, &[], &[]);
        small.max_bytes = 10;
        let outcome = reconcile_instruction_context(&small, &mut VersionCache::new(), &mut seam);
        assert!(outcome.is_none(), "notice-only renders commit nothing");
    }

    #[test]
    fn baseline_state_builds_changes_and_versions() {
        let files = vec![LoadedInstructionFile {
            absolute_path: "/w/proj/AGENTS.md".to_string(),
            display_path: "AGENTS.md".to_string(),
            content: "guide".to_string(),
            version: Some("v1".to_string()),
        }];
        let (changes, versions) = baseline_instruction_state(&files);
        let scope = candidate_scope_key(".", "AGENTS.md");
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes.get(&scope).expect("scoped").action,
            InstructionChangeAction::Set
        );
        assert!(versions.contains_key(&scope));
    }

    #[test]
    fn parsing_tolerates_unknown_shapes() {
        let mut source = serde_json::Map::new();
        source.insert("kind".to_string(), "agent-instructions".into());
        source.insert(
            "changes".to_string(),
            serde_json::json!([
                {"action": "set", "scope": "pkg\u{0}AGENTS.md", "path": "pkg/AGENTS.md"},
                {"action": "explode"},
                "not an object",
                {"action": "set", "scope": 4, "path": "x"},
                {"action": "set", "scope": "pkg\u{0}X.md", "path": "x", "digest": 7},
            ]),
        );
        let parsed = workspace_instruction_changes(&source);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].scope, "pkg\u{0}AGENTS.md");
    }
}
