// Rust guideline compliant 2026-11-02
//! The local filesystem skill provider (dsh
//! `@deepseek-ai/dsh-skill-filesystem`, 1:1).
//!
//! Project, custom, user, and bundled skill roots discover
//! directory-bundle and flat Markdown skills, parse YAML frontmatter,
//! and load bodies through the local filesystem domain.
//!
//! One fr carrier adaptation (19.2 entry 32): the dsh chokidar
//! root/ancestor watch state machine is a single-thread signature
//! poller -- per-root existence plus a shallow mtime signature, with a
//! change observed only after it stays stable for
//! `watch-stability-threshold-ms`. The owner bookkeeping
//! (`shared:`/`project:` owners, `watch-max-projects` insertion-order
//! eviction) is 1:1. Brand mappings ride 19.2 entry 33: project roots
//! are `.fr/skills` and user root `<FR_HOME>/skills`; `.agents/skills`
//! and the agents home stay verbatim.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Instant, SystemTime};

use serde_json::Value;

use crate::fs_domain::FsDomain;
use crate::fs_types::{FsError, FsErrorCode, FsKind};
use crate::skill_runtime::{
    BUNDLED_SKILL_RANK, SkillCandidate, SkillDefinition, SkillInvocationPolicy, SkillLocator,
    SkillLookupOptions, SkillProvider, SkillProviderOutput, SkillResourceBase, is_skill_name,
};

/// The project `.fr/skills` root rank (dsh `PROJECT_DSH_RANK`).
pub const PROJECT_FR_RANK: i64 = 100;

/// The project `.agents/skills` root rank (dsh `PROJECT_AGENTS_RANK`).
pub const PROJECT_AGENTS_RANK: i64 = 200;

/// The custom skill directory rank (dsh `CUSTOM_RANK`).
pub const CUSTOM_RANK: i64 = 300;

/// The user `<FR_HOME>/skills` root rank (dsh `USER_DSH_RANK`).
pub const USER_FR_RANK: i64 = 400;

/// The user agents-home skills root rank (dsh `USER_AGENTS_RANK`).
pub const USER_AGENTS_RANK: i64 = 500;

/// The default observed-change stability window in milliseconds (dsh
/// `DEFAULT_WATCH_STABILITY_THRESHOLD_MS`).
pub const DEFAULT_WATCH_STABILITY_THRESHOLD_MS: u64 = 200;

/// The default signature-poll cadence in milliseconds (dsh
/// `DEFAULT_WATCH_POLL_INTERVAL_MS`).
pub const DEFAULT_WATCH_POLL_INTERVAL_MS: u64 = 100;

/// The default distinct-project-root watch bound (dsh
/// `DEFAULT_WATCH_MAX_PROJECTS`).
pub const DEFAULT_WATCH_MAX_PROJECTS: usize = 128;

/// The provider configuration (dsh `Config`, the 12 keys kebab-mapped;
/// M1 carries the baked base-composition defaults).
#[expect(
    clippy::struct_excessive_bools,
    reason = "the dsh Config face is 1:1 with four independent boolean keys; grouping them would break the config shape"
)]
#[derive(Debug, Clone)]
pub struct FileSystemProviderConfig {
    /// Unique provider name (dsh `providerName`).
    pub provider_name: String,
    /// Whether project and user roots are included around custom roots
    /// (dsh `includeDefaultRoots`).
    pub include_default_roots: bool,
    /// Harness home override (dsh `dshHome`; resolved through the
    /// `ShellEnvRegistry` chain per 19.2 entry 33).
    pub fr_home: Option<String>,
    /// Shared agent config root override (dsh `agentsHome`).
    pub agents_home: Option<String>,
    /// Additional skill roots scanned after project roots and before
    /// user roots (dsh `customSkillDirs`).
    pub custom_skill_dirs: Vec<String>,
    /// Whether skill roots are watched for catalog changes (dsh
    /// `watch`).
    pub watch: bool,
    /// Carried key (dsh `watchUsePolling`); the fr watcher is always
    /// the signature poller, so this has no arm to select.
    pub watch_use_polling: bool,
    /// Milliseconds a changed skill entry must remain stable before it
    /// is observed (dsh `watchStabilityThresholdMs`).
    pub watch_stability_threshold_ms: u64,
    /// Milliseconds between signature probes (dsh
    /// `watchPollIntervalMs`).
    pub watch_poll_interval_ms: u64,
    /// Maximum distinct project roots whose skill directories remain
    /// watched (dsh `watchMaxProjects`).
    pub watch_max_projects: usize,
    /// Carried key (dsh `watchFollowSymlinks`): whether discovery
    /// follows a symlinked skill root.
    pub watch_follow_symlinks: bool,
    /// Bundled skill root override (dsh `bundledSkillDir`; absent
    /// reads `FR_BUNDLED_SKILL_DIR` when default roots are included).
    pub bundled_skill_dir: Option<String>,
}

impl Default for FileSystemProviderConfig {
    fn default() -> Self {
        Self {
            provider_name: "filesystem".to_owned(),
            include_default_roots: true,
            fr_home: None,
            agents_home: None,
            custom_skill_dirs: Vec::new(),
            watch: true,
            watch_use_polling: false,
            watch_stability_threshold_ms: DEFAULT_WATCH_STABILITY_THRESHOLD_MS,
            watch_poll_interval_ms: DEFAULT_WATCH_POLL_INTERVAL_MS,
            watch_max_projects: DEFAULT_WATCH_MAX_PROJECTS,
            watch_follow_symlinks: true,
            bundled_skill_dir: None,
        }
    }
}

/// The positive-integer refusal (dsh `assertPositiveInteger` with the
/// skill-filesystem prefix; the text is verbatim).
fn assert_positive_integer(name: &str, value: u64) -> Result<(), String> {
    if value < 1 {
        return Err(format!(
            "skill-filesystem: {name} must be a positive integer"
        ));
    }
    Ok(())
}

/// The resolved runtime configuration (the constructor-facing form).
#[derive(Debug, Clone)]
struct ResolvedProviderConfig {
    name: String,
    include_default_roots: bool,
    fr_home: String,
    agents_home: String,
    custom_skill_dirs: Vec<PathBuf>,
    watch_enabled: bool,
    stability_threshold_ms: u64,
    poll_interval_ms: u64,
    max_projects: usize,
    bundled_skill_dir: Option<PathBuf>,
}

/// The environment lookup seam (production reads the process
/// environment; tests script it).
type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Resolves the configuration into its runtime form (dsh the Config
/// schema defaults plus the constructor resolution).
///
/// # Errors
///
/// The positive-integer watch refusals, or an unresolvable harness
/// home.
fn resolve_config(
    config: &FileSystemProviderConfig,
    env: EnvLookup<'_>,
) -> Result<ResolvedProviderConfig, String> {
    assert_positive_integer(
        "watchStabilityThresholdMs",
        config.watch_stability_threshold_ms,
    )?;
    assert_positive_integer("watchPollIntervalMs", config.watch_poll_interval_ms)?;
    assert_positive_integer(
        "watchMaxProjects",
        u64::try_from(config.watch_max_projects).unwrap_or(0),
    )?;
    let fr_home = crate::shell_env::resolve_home(config.fr_home.as_deref())?;
    let agents_home = resolve_agents_home(config.agents_home.as_deref(), env);
    let custom_skill_dirs = config
        .custom_skill_dirs
        .iter()
        .map(|dir| Path::new(dir).to_path_buf())
        .collect();
    // The environment bundled root is a default root: an isolated
    // provider must see only its explicit roots (the dsh constructor
    // comment verbatim in intent).
    let bundled = config.bundled_skill_dir.clone().or_else(|| {
        if config.include_default_roots {
            env("FR_BUNDLED_SKILL_DIR")
        } else {
            None
        }
    });
    Ok(ResolvedProviderConfig {
        name: config.provider_name.clone(),
        include_default_roots: config.include_default_roots,
        fr_home,
        agents_home,
        custom_skill_dirs,
        watch_enabled: config.watch,
        stability_threshold_ms: config.watch_stability_threshold_ms,
        poll_interval_ms: config.watch_poll_interval_ms,
        max_projects: config.watch_max_projects,
        bundled_skill_dir: bundled.map(|dir| Path::new(&dir).to_path_buf()),
    })
}

/// Resolves the shared agent config root (dsh `resolveAgentsHome`):
/// the explicit override, then `FR_AGENTS_HOME`, then `~/.agents` (the
/// cross-tool conventional directory stays verbatim, 19.2 entry 33).
fn resolve_agents_home(override_home: Option<&str>, env: EnvLookup<'_>) -> String {
    if let Some(home) = override_home {
        return home.to_owned();
    }
    if let Some(home) = env("FR_AGENTS_HOME") {
        return home;
    }
    let user_home = env("HOME").unwrap_or_else(|| ".".to_owned());
    format!("{user_home}/.agents")
}

/// One discovered skill root (dsh `SkillRoot`).
#[derive(Debug, Clone)]
struct SkillRoot {
    path: PathBuf,
    source: &'static str,
    rank: i64,
    skip_system: bool,
    project_root: Option<PathBuf>,
    trusted_host: bool,
}

/// One directory child candidate (dsh `SkillRootEntry`).
struct SkillRootEntry {
    name: String,
    kind: EntryKind,
    path: PathBuf,
}

/// The entry-kind probe outcome (dsh `'directory' | 'file' | other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Other,
}

/// A parsed skill file before candidate assembly (dsh `ParsedSkill`).
struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    invocation: SkillInvocationPolicy,
    metadata: Option<Value>,
    path: String,
    content: String,
}

/// Resolves the root set for one workspace (dsh `roots`, the order and
/// ranks 1:1 over the 19.2 entry 33 names).
fn resolve_roots(resolved: &ResolvedProviderConfig, cwd: Option<&str>) -> Vec<SkillRoot> {
    let mut roots = Vec::new();
    let project_cwd = if resolved.include_default_roots {
        cwd
    } else {
        None
    };
    {
        if let Some(cwd) = project_cwd {
            let project_root = find_project_root(Path::new(cwd));
            roots.push(SkillRoot {
                path: project_root.join(".fr/skills"),
                source: "project-fr",
                rank: PROJECT_FR_RANK,
                skip_system: false,
                project_root: Some(project_root.clone()),
                trusted_host: false,
            });
            roots.push(SkillRoot {
                path: project_root.join(".agents/skills"),
                source: "project-agents",
                rank: PROJECT_AGENTS_RANK,
                skip_system: false,
                project_root: Some(project_root),
                trusted_host: false,
            });
        }
    }
    for dir in &resolved.custom_skill_dirs {
        roots.push(SkillRoot {
            path: dir.clone(),
            source: "custom",
            rank: CUSTOM_RANK,
            skip_system: false,
            project_root: None,
            trusted_host: false,
        });
    }
    if resolved.include_default_roots {
        roots.push(SkillRoot {
            path: Path::new(&resolved.fr_home).join("skills"),
            source: "user-fr",
            rank: USER_FR_RANK,
            skip_system: true,
            project_root: None,
            trusted_host: false,
        });
        roots.push(SkillRoot {
            path: Path::new(&resolved.agents_home).join("skills"),
            source: "user-agents",
            rank: USER_AGENTS_RANK,
            skip_system: false,
            project_root: None,
            trusted_host: false,
        });
    }
    if let Some(bundled) = &resolved.bundled_skill_dir {
        roots.push(SkillRoot {
            path: bundled.clone(),
            source: "bundled",
            rank: BUNDLED_SKILL_RANK,
            skip_system: false,
            project_root: None,
            trusted_host: true,
        });
    }
    roots
}

/// Walks upward to the enclosing `.git` anchor (dsh
/// `findProjectRoot`); the cwd itself when no anchor exists.
fn find_project_root(cwd: &Path) -> PathBuf {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return current;
        }
        let Some(parent) = current.parent() else {
            return cwd.to_path_buf();
        };
        if parent == current {
            return cwd.to_path_buf();
        }
        current = parent.to_path_buf();
    }
}

/// The frontmatter split outcome (dsh `parseFrontmatter`): the YAML
/// data mapping plus the trailing body, or `None` for the missing
/// frontmatter shape (no `---` opener, no closer, or a non-mapping
/// document).
fn parse_frontmatter(raw: &str) -> Result<Option<(serde_yaml_ng::Mapping, String)>, String> {
    let Some((yaml, body)) = split_frontmatter(raw) else {
        return Ok(None);
    };
    let parsed: serde_yaml_ng::Value = match serde_yaml_ng::from_str(yaml) {
        Ok(parsed) => parsed,
        Err(error) => return Err(error.to_string()),
    };
    let Some(mapping) = parsed.as_mapping() else {
        return Ok(None);
    };
    Ok(Some((mapping.clone(), body.to_owned())))
}

/// Splits the raw file at the `---` fences (dsh `findClosingFrontmatter`
/// arm of `parseFrontmatter`); `None` when either fence is missing.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let first_line_end = raw.find('\n')?;
    let head = &raw[..first_line_end];
    let first_line = head.strip_suffix('\r').unwrap_or(head);
    if first_line != "---" {
        return None;
    }
    let mut line_start = first_line_end + 1;
    while line_start <= raw.len() {
        let rest = &raw[line_start..];
        let next_newline = rest.find('\n');
        let line_end = line_start + next_newline.unwrap_or(rest.len());
        let slice = &raw[line_start..line_end];
        let line = slice.strip_suffix('\r').unwrap_or(slice);
        if line == "---" {
            let body_start = next_newline.map_or(raw.len(), |offset| line_start + offset + 1);
            return Some((&raw[first_line_end + 1..line_start], &raw[body_start..]));
        }
        line_start = line_end + 1;
    }
    None
}

/// Reads one required non-empty string field (dsh `stringField`).
fn string_field(data: &serde_yaml_ng::Mapping, key: &str) -> Option<String> {
    let value = data.get(serde_yaml_ng::Value::String(key.to_owned()))?;
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Reads one optional non-empty string field (dsh `optionalString`).
fn optional_string_field(data: &serde_yaml_ng::Mapping, key: &str) -> Option<String> {
    string_field(data, key)
}

/// Rejects one legacy invocation key with the dsh refusal verbatim.
fn reject_legacy_invocation_key(
    data: &serde_yaml_ng::Mapping,
    legacy: &str,
    canonical: &str,
) -> Result<(), String> {
    let key = serde_yaml_ng::Value::String(legacy.to_owned());
    if data.contains_key(&key) {
        return Err(format!(
            "frontmatter field \"{legacy}\" is unsupported; use \"{canonical}\""
        ));
    }
    Ok(())
}

/// Reads one frontmatter boolean under the dsh value family (dsh
/// `frontmatterBoolean`): booleans, `1`/`0`, `'1'`/`'0'`, and the
/// case-insensitive `true/yes/on` and `false/no/off` words.
///
/// # Errors
///
/// The `frontmatter field "{key}" must be a boolean` refusal verbatim.
fn frontmatter_boolean(data: &serde_yaml_ng::Mapping, key: &str) -> Result<Option<bool>, String> {
    let key_value = serde_yaml_ng::Value::String(key.to_owned());
    if !data.contains_key(&key_value) {
        return Ok(None);
    }
    let value = data.get(&key_value).expect("present");
    if let Some(flag) = value.as_bool() {
        return Ok(Some(flag));
    }
    if value == &serde_yaml_ng::Value::Number(1.into()) || value.as_str() == Some("1") {
        return Ok(Some(true));
    }
    if value == &serde_yaml_ng::Value::Number(0.into()) || value.as_str() == Some("0") {
        return Ok(Some(false));
    }
    if let Some(word) = value.as_str() {
        match word.to_lowercase().as_str() {
            "true" | "yes" | "on" => return Ok(Some(true)),
            "false" | "no" | "off" => return Ok(Some(false)),
            _ => {}
        }
    }
    Err(format!("frontmatter field \"{key}\" must be a boolean"))
}

/// Parses the invocation controls (dsh `parseInvocationPolicy`): the
/// legacy keys are refused, then the canonical booleans default the
/// invocability.
///
/// # Errors
///
/// The legacy-key and boolean-family refusals verbatim.
fn parse_invocation_policy(data: &serde_yaml_ng::Mapping) -> Result<SkillInvocationPolicy, String> {
    reject_legacy_invocation_key(data, "disableModelInvocation", "disable-model-invocation")?;
    reject_legacy_invocation_key(data, "modelInvocable", "disable-model-invocation")?;
    reject_legacy_invocation_key(data, "userInvocable", "user-invocable")?;
    let disable_model_invocation = frontmatter_boolean(data, "disable-model-invocation")?;
    let user_invocable = frontmatter_boolean(data, "user-invocable")?;
    Ok(SkillInvocationPolicy {
        model_invocable: disable_model_invocation != Some(true),
        user_invocable: user_invocable != Some(false),
    })
}

/// Reads one optional object metadata field (dsh `optionalMetadata`):
/// mappings only, arrays and scalars ignored.
fn optional_metadata(data: &serde_yaml_ng::Mapping) -> Option<Value> {
    let value = data.get(serde_yaml_ng::Value::String("metadata".to_owned()))?;
    if value.is_mapping() {
        return serde_json::to_value(value).ok();
    }
    None
}

/// Parses one skill file body into its fields (dsh `parseSkillFile`
/// validation family; the absent-file arm is the caller's `None`).
///
/// # Errors
///
/// The warn-family reasons are carried as the `Err` text for the
/// caller's `skill file {path} ignored: {reason}` warning.
fn validate_parsed_fields(data: &serde_yaml_ng::Mapping) -> Result<ParsedFields, String> {
    let Some(name) = string_field(data, "name") else {
        return Err("frontmatter requires name and description".to_owned());
    };
    let Some(description) = string_field(data, "description") else {
        return Err("frontmatter requires name and description".to_owned());
    };
    if !is_skill_name(&name) {
        return Err(format!("invalid skill name \"{name}\""));
    }
    let invocation = parse_invocation_policy(data)
        .map_err(|error| format!("invalid invocation frontmatter: {error}"))?;
    Ok(ParsedFields {
        name,
        description,
        when_to_use: optional_string_field(data, "whenToUse"),
        invocation,
        metadata: optional_metadata(data),
    })
}

/// The validated frontmatter fields (dsh the `ParsedSkill` field
/// family before the body and path join).
struct ParsedFields {
    name: String,
    description: String,
    when_to_use: Option<String>,
    invocation: SkillInvocationPolicy,
    metadata: Option<Value>,
}

/// The raw skill text with its display-path spelling (dsh
/// `SkillText`).
struct SkillText {
    path: String,
    content: String,
}

/// Whether one filesystem error is the absent family (dsh
/// `isAbsentPathError`: `ENOENT`/`ENOTDIR`).
fn is_absent_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// Whether one typed error is the absent family (dsh
/// `isAbsentSkillPathError`: the io absent codes plus the domain's
/// `FS_NOT_FOUND`/`FS_NOT_DIRECTORY`).
const fn is_absent_code(error: &FsError) -> bool {
    matches!(
        error.code,
        FsErrorCode::NotFound | FsErrorCode::NotDirectory
    )
}

/// The error text for the warn faces (dsh `errorMessage`): the typed
/// code word plus the message.
fn error_text(error: &FsError) -> String {
    format!("{}: {}", error.code.as_str(), error.message)
}

/// Reads the raw skill text (dsh `readSkillText`): the filesystem
/// domain face for provider-owned roots and the trusted-host std face
/// for the bundled root (lossy text, no `FS_NOT_TEXT` arm, matching
/// the Node utf8 read).
///
/// # Errors
///
/// Non-absent read failures propagate (the registry skips the
/// provider).
fn read_skill_text(
    fs: &FsDomain,
    path: &Path,
    trusted_host: bool,
) -> Result<Option<SkillText>, String> {
    if !trusted_host {
        return read_skill_text_from_domain(fs, path);
    }
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) => {
            if is_absent_error(&error) {
                return Ok(None);
            }
            return Err(error.to_string());
        }
    };
    match std::fs::read(&resolved) {
        Ok(bytes) => Ok(Some(SkillText {
            path: resolved.display().to_string(),
            content: String::from_utf8_lossy(&bytes).into_owned(),
        })),
        Err(error) => {
            if is_absent_error(&error) {
                return Ok(None);
            }
            Err(error.to_string())
        }
    }
}

/// The filesystem-domain read face (dsh `readSkillTextFromFileSystem`):
/// resolve, stat the file kind, read the text; absent codes return
/// `None`, a non-text file warns and returns `None`.
///
/// # Errors
///
/// Non-absent failures propagate.
fn read_skill_text_from_domain(fs: &FsDomain, path: &Path) -> Result<Option<SkillText>, String> {
    let path_text = path.display().to_string();
    let target = match fs.resolve(&path_text, None) {
        Ok(target) => target,
        Err(error) => {
            if is_absent_code(&error) {
                return Ok(None);
            }
            return Err(error_text(&error));
        }
    };
    let info = match fs.stat(&target) {
        Ok(Some(info)) => info,
        Ok(None) => return Ok(None),
        Err(error) => {
            if is_absent_code(&error) {
                return Ok(None);
            }
            return Err(error_text(&error));
        }
    };
    if info.kind != FsKind::File {
        return Ok(None);
    }
    match fs.read_text(&target) {
        Ok(content) => Ok(Some(SkillText {
            path: FsDomain::process_path(&target),
            content,
        })),
        Err(error) => {
            if is_absent_code(&error) {
                return Ok(None);
            }
            if error.code == FsErrorCode::NotText {
                eprintln!(
                    "skill file {path_text} ignored: failed to read text file at {}: {}",
                    FsDomain::process_path(&target),
                    error_text(&error)
                );
                return Ok(None);
            }
            Err(error_text(&error))
        }
    }
}

/// Parses one skill file into its fields (dsh `parseSkillFile`): the
/// absent arm is `Ok(None)`; every validation miss warns the dsh text
/// and returns `Ok(None)`.
///
/// # Errors
///
/// Non-absent read failures propagate.
fn parse_skill_file(
    fs: &FsDomain,
    path: &Path,
    trusted_host: bool,
) -> Result<Option<ParsedSkill>, String> {
    let Some(raw) = read_skill_text(fs, path, trusted_host)? else {
        return Ok(None);
    };
    let sections = match parse_frontmatter(&raw.content) {
        Ok(sections) => sections,
        Err(error) => {
            eprintln!(
                "skill file {} ignored: invalid YAML frontmatter: {error}",
                raw.path
            );
            return Ok(None);
        }
    };
    let Some((data, body)) = sections else {
        eprintln!("skill file {} ignored: missing YAML frontmatter", raw.path);
        return Ok(None);
    };
    let fields = match validate_parsed_fields(&data) {
        Ok(fields) => fields,
        Err(reason) => {
            eprintln!("skill file {} ignored: {reason}", raw.path);
            return Ok(None);
        }
    };
    Ok(Some(ParsedSkill {
        name: fields.name,
        description: fields.description,
        when_to_use: fields.when_to_use,
        invocation: fields.invocation,
        metadata: fields.metadata,
        path: raw.path,
        content: body.trim().to_owned(),
    }))
}

/// Discovers one root's skill candidates (dsh `discoverRoot`): the
/// entries sort by name (the localeCompare carrier is the case-folded
/// byte compare), directory bundles and flat `.md` files map to
/// locators, and every other shape is skipped.
///
/// # Errors
///
/// Non-absent listing or read failures propagate.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the dsh endsWith('.md') probe is case-sensitive verbatim; a case-insensitive compare would accept files dsh rejects"
)]
fn discover_root(
    root: &SkillRoot,
    fs: &FsDomain,
    provider: &str,
) -> Result<Vec<SkillCandidate>, String> {
    let mut skills = Vec::new();
    let mut entries = list_skill_root_entries(root, fs)?;
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    for entry in entries {
        if root.skip_system && entry.name == ".system" {
            continue;
        }
        let locator = match entry.kind {
            EntryKind::Directory => SkillLocator::Local {
                path: entry.path.join("SKILL.md").display().to_string(),
                directory: entry.path.display().to_string(),
            },
            EntryKind::File if entry.name.ends_with(".md") => SkillLocator::Local {
                path: entry.path.display().to_string(),
                directory: root.path.display().to_string(),
            },
            _ => continue,
        };
        let SkillLocator::Local { path, directory } = &locator else {
            continue;
        };
        let Some(parsed) = parse_skill_file(fs, Path::new(path), root.trusted_host)? else {
            continue;
        };
        skills.push(SkillCandidate {
            path: Some(parsed.path),
            name: parsed.name,
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            invocation: parsed.invocation,
            source: root.source.to_owned(),
            provider: provider.to_owned(),
            resource_base: Some(SkillResourceBase::Directory {
                path: directory.clone(),
            }),
            rank: root.rank,
            locator,
            metadata: parsed.metadata,
        });
    }
    Ok(skills)
}

/// Lists one root's children (dsh `listSkillRootEntries`): the domain
/// face for provider-owned roots, the std face for the trusted bundled
/// root; the absent family is an empty list.
///
/// # Errors
///
/// Non-absent listing failures propagate.
fn list_skill_root_entries(root: &SkillRoot, fs: &FsDomain) -> Result<Vec<SkillRootEntry>, String> {
    if !root.trusted_host {
        let root_text = root.path.display().to_string();
        let target = match fs.resolve(&root_text, None) {
            Ok(target) => target,
            Err(error) => {
                if is_absent_code(&error) {
                    return Ok(Vec::new());
                }
                return Err(error_text(&error));
            }
        };
        let entries = match fs.list_dir(&target) {
            Ok(entries) => entries,
            Err(error) => {
                if is_absent_code(&error) {
                    return Ok(Vec::new());
                }
                return Err(error_text(&error));
            }
        };
        return Ok(entries
            .into_iter()
            .map(|entry| SkillRootEntry {
                name: entry.name,
                kind: match entry.kind {
                    FsKind::File => EntryKind::File,
                    FsKind::Directory => EntryKind::Directory,
                    FsKind::Other => EntryKind::Other,
                },
                path: PathBuf::from(entry.target.display_path),
            })
            .collect());
    }
    list_skill_root_entries_from_std(root)
}

/// The std trusted-host listing face (dsh
/// `listSkillRootEntriesFromNode`): directory and file entries keep
/// their kinds, symlinks resolve through a metadata follow, and every
/// other shape degrades to `other`.
///
/// # Errors
///
/// Non-absent listing failures propagate.
fn list_skill_root_entries_from_std(root: &SkillRoot) -> Result<Vec<SkillRootEntry>, String> {
    let read = match std::fs::read_dir(&root.path) {
        Ok(read) => read,
        Err(error) => {
            if is_absent_error(&error) {
                return Ok(Vec::new());
            }
            return Err(error.to_string());
        }
    };
    let mut entries = Vec::new();
    for entry in read {
        let Ok(entry) = entry else {
            continue;
        };
        let path = root.path.join(entry.file_name());
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_symlink() {
            match std::fs::metadata(&path) {
                Ok(info) if info.is_dir() => EntryKind::Directory,
                Ok(info) if info.is_file() => EntryKind::File,
                Ok(_) => EntryKind::Other,
                Err(error) => {
                    eprintln!(
                        "skill entry {} ignored: failed to follow symbolic link: {error}",
                        path.display()
                    );
                    continue;
                }
            }
        } else {
            EntryKind::Other
        };
        entries.push(SkillRootEntry {
            name: entry.file_name().display().to_string(),
            kind,
            path,
        });
    }
    Ok(entries)
}

/// The provider-side watch manager (dsh `SkillWatchManager`): the
/// owner bookkeeping is 1:1 while the per-root chokidar watchers are
/// one signature-poll thread (19.2 entry 32).
struct WatchManager {
    shared: Arc<WatchShared>,
    spawn_ok: bool,
}

/// The state one poll thread serves (the thread owns an `Arc` of this
/// only, so a provider drop cannot race a late tick).
struct WatchShared {
    invalidator: Weak<crate::skill_runtime::SkillRegistry>,
    state: Mutex<WatchState>,
    stop: AtomicBool,
    enabled: bool,
    stability_threshold_ms: u64,
    max_projects: usize,
}

/// The accumulated watch bookkeeping (dsh the manager's `roots` and
/// `projects` maps: shared roots accumulate, project roots are
/// insertion-ordered by observation for the eviction).
#[derive(Default)]
struct WatchState {
    roots: Vec<SkillRoot>,
    projects: Vec<(PathBuf, Vec<PathBuf>)>,
    signatures: std::collections::HashMap<String, RootSignature>,
    pending: std::collections::HashMap<String, (RootSignature, Instant)>,
}

/// The per-root shallow signature (19.2 entry 32): root existence,
/// the immediate entries (name, directory flag, mtime; `.system`
/// excluded on skip-system roots), and each directory entry's
/// `SKILL.md` mtime -- the discovery-read surface, two levels deep.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RootSignature {
    alive: bool,
    entries: Vec<(String, bool, Option<SystemTime>)>,
    skill_bodies: Vec<(String, Option<SystemTime>)>,
}

impl WatchManager {
    /// Assembles the manager and starts the poll thread (dsh the
    /// manager constructor plus the watcher start; a spawn failure is
    /// the watch-startup failure and surfaces as incomplete
    /// discovery).
    fn new(
        invalidator: Weak<crate::skill_runtime::SkillRegistry>,
        resolved: &ResolvedProviderConfig,
    ) -> Self {
        let shared = Arc::new(WatchShared {
            invalidator,
            state: Mutex::new(WatchState::default()),
            stop: AtomicBool::new(false),
            enabled: resolved.watch_enabled,
            stability_threshold_ms: resolved.stability_threshold_ms,
            max_projects: resolved.max_projects,
        });
        let thread_shared = Arc::clone(&shared);
        let interval = std::time::Duration::from_millis(resolved.poll_interval_ms);
        let spawn = std::thread::Builder::new()
            .name("skill-filesystem watcher".to_owned())
            .spawn(move || {
                loop {
                    if thread_shared.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    thread_shared.tick(Instant::now());
                    std::thread::sleep(interval);
                }
            });
        Self {
            shared,
            spawn_ok: spawn.is_ok(),
        }
    }

    /// Retains the observation's owners and evicts over-cap projects
    /// (dsh `observeRoots`); the completion face is the manager's.
    fn observe_roots(&self, roots: &[SkillRoot]) -> bool {
        self.shared.observe_roots(roots);
        self.shared.enabled && self.spawn_ok
    }
}

impl WatchShared {
    /// The owner bookkeeping (dsh `observeRoots`): shared roots
    /// accumulate forever, project roots reinsert to the newest slot,
    /// and the oldest project's roots drop with their signatures; an
    /// eviction invalidates.
    fn observe_roots(&self, roots: &[SkillRoot]) {
        let mut state = self.state.lock().expect("watch state");
        for root in roots.iter().filter(|root| root.project_root.is_none()) {
            if !state.roots.iter().any(|kept| kept.path == root.path) {
                state.roots.push(root.clone());
            }
        }
        let mut grouped: Vec<(PathBuf, Vec<SkillRoot>)> = Vec::new();
        for root in roots.iter().filter_map(|root| {
            root.project_root
                .clone()
                .map(|project_root| (project_root, root.clone()))
        }) {
            let slot = grouped
                .iter_mut()
                .find(|(project_root, _)| *project_root == root.0);
            match slot {
                Some((_, group)) => group.push(root.1),
                None => grouped.push((root.0, vec![root.1])),
            }
        }
        for (project_root, group) in grouped {
            state.projects.retain(|(kept, _)| *kept != project_root);
            let paths: Vec<PathBuf> = group.iter().map(|root| root.path.clone()).collect();
            for root in &group {
                if !state.roots.iter().any(|kept| kept.path == root.path) {
                    state.roots.push(root.clone());
                }
            }
            state.projects.push((project_root, paths));
        }
        let mut evicted = false;
        while state.projects.len() > self.max_projects {
            let (_, paths) = state.projects.remove(0);
            for path in paths {
                state.roots.retain(|kept| kept.path != path);
                let key = path.display().to_string();
                state.signatures.remove(&key);
                state.pending.remove(&key);
            }
            evicted = true;
        }
        drop(state);
        if evicted {
            self.invalidate();
        }
    }
}

impl WatchShared {
    /// Runs one poll tick (dsh the chokidar event pipeline folded into
    /// the signature ladder): a new root's baseline adopts without an
    /// invalidation, a reverted change drops the pending record, and a
    /// change stable for the threshold window invalidates. Returns
    /// whether the registry was invalidated.
    fn tick(&self, now: Instant) -> bool {
        if !self.enabled {
            return false;
        }
        let mut invalidated = false;
        let mut state = self.state.lock().expect("watch state");
        let roots = state.roots.clone();
        for root in roots {
            let key = root.path.display().to_string();
            let current = compute_signature(&root);
            let Some(stable) = state.signatures.get(&key) else {
                state.signatures.insert(key, current);
                continue;
            };
            if *stable == current {
                state.pending.remove(&key);
                continue;
            }
            match state.pending.get(&key) {
                Some((pending, since))
                    if *pending == current
                        && now.duration_since(*since)
                            >= std::time::Duration::from_millis(self.stability_threshold_ms) =>
                {
                    state.pending.remove(&key);
                    state.signatures.insert(key, current);
                    invalidated = true;
                }
                Some((pending, _)) if *pending == current => {}
                _ => {
                    state.pending.insert(key, (current, now));
                }
            }
        }
        drop(state);
        if invalidated {
            self.invalidate();
        }
        invalidated
    }

    /// Invalidates the registry through the weak handle (the fr
    /// carrier of dsh `control.invalidate`).
    fn invalidate(&self) {
        if let Some(registry) = self.invalidator.upgrade() {
            registry.invalidate();
        }
    }

    /// Invalidates when one first-party mutation touches a watched
    /// potential skill path (dsh `observeHostMutation`).
    fn observe_host_mutation(&self, path: &Path) {
        if self.stop.load(Ordering::SeqCst) {
            return;
        }
        if self.is_watched_mutation(path) {
            self.invalidate();
        }
    }

    /// Whether one absolute path touches a watched potential skill
    /// path (the mutation gate; the invalidation is its one-line
    /// consumer).
    fn is_watched_mutation(&self, path: &Path) -> bool {
        let state = self.state.lock().expect("watch state");
        state
            .roots
            .iter()
            .any(|root| is_potential_skill_path(root, path))
    }
}

/// Computes one root's shallow signature (host std face; the dsh
/// watchers observe the host filesystem directly, and so does the
/// poller).
fn compute_signature(root: &SkillRoot) -> RootSignature {
    let Ok(info) = std::fs::metadata(&root.path) else {
        return RootSignature {
            alive: false,
            entries: Vec::new(),
            skill_bodies: Vec::new(),
        };
    };
    if !info.is_dir() {
        return RootSignature {
            alive: false,
            entries: Vec::new(),
            skill_bodies: Vec::new(),
        };
    }
    let mut entries = Vec::new();
    let mut skill_bodies = Vec::new();
    let Ok(read) = std::fs::read_dir(&root.path) else {
        return RootSignature {
            alive: true,
            entries: Vec::new(),
            skill_bodies: Vec::new(),
        };
    };
    for entry in read.flatten() {
        let name = entry.file_name().display().to_string();
        if root.skip_system && name == ".system" {
            continue;
        }
        let path = root.path.join(entry.file_name());
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta.modified().ok();
        if meta.is_dir() {
            entries.push((name.clone(), true, modified));
            skill_bodies.push((
                name,
                std::fs::metadata(path.join("SKILL.md"))
                    .and_then(|meta| meta.modified())
                    .ok(),
            ));
        } else {
            entries.push((name, false, modified));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    skill_bodies.sort_by(|left, right| left.0.cmp(&right.0));
    RootSignature {
        alive: true,
        entries,
        skill_bodies,
    }
}

/// Whether one absolute path is a watched potential skill path (dsh
/// `isPotentialSkillPath`): at most two segments under the root, the
/// skip-system branch respected, a flat `.md` entry or a directory's
/// `SKILL.md`.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "the dsh endsWith('.md') probe is case-sensitive verbatim"
)]
fn is_potential_skill_path(root: &SkillRoot, path: &Path) -> bool {
    let Some(segments) = contained_segments(&root.path, path) else {
        return false;
    };
    if segments.is_empty() || segments.len() > 2 {
        return false;
    }
    if root.skip_system && segments[0] == ".system" {
        return false;
    }
    if segments.len() == 1 {
        return segments[0].ends_with(".md");
    }
    segments[1] == "SKILL.md"
}

/// The root-relative segments of one path (dsh `containedSegments`):
/// `None` when the path escapes the root.
fn contained_segments(root: &Path, path: &Path) -> Option<Vec<String>> {
    let relative = path.strip_prefix(root).ok()?;
    Some(
        relative
            .components()
            .map(|segment| segment.as_os_str().display().to_string())
            .collect(),
    )
}

/// Provider that maps local project/user skill roots into the
/// registry (dsh `FileSystemSkillProvider`).
pub struct FileSystemSkillProvider {
    name: String,
    resolved: ResolvedProviderConfig,
    fs: Arc<FsDomain>,
    watch: WatchManager,
}

impl FileSystemSkillProvider {
    /// Assembles the provider over the resolved configuration and
    /// starts the watch thread.
    ///
    /// # Errors
    ///
    /// An unresolvable harness home.
    pub fn new(
        invalidator: Weak<crate::skill_runtime::SkillRegistry>,
        fs: Arc<FsDomain>,
        config: &FileSystemProviderConfig,
        env: EnvLookup<'_>,
    ) -> Result<Self, String> {
        let resolved = resolve_config(config, env)?;
        let name = resolved.name.clone();
        let watch = WatchManager::new(invalidator, &resolved);
        Ok(Self {
            name,
            resolved,
            fs,
            watch,
        })
    }

    /// Invalidates this provider synchronously after a first-party
    /// filesystem mutation (dsh `observeHostMutation`; the fs
    /// write/edit tools call this in-process, the dsh `fs/observed`
    /// actor filter's in-crate equivalent).
    pub fn observe_host_mutation(&self, path: &str) {
        self.watch.shared.observe_host_mutation(Path::new(path));
    }
}

impl std::fmt::Debug for FileSystemSkillProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileSystemSkillProvider")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Drop for FileSystemSkillProvider {
    fn drop(&mut self) {
        self.watch.shared.stop.store(true, Ordering::SeqCst);
    }
}

impl SkillProvider for FileSystemSkillProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn list(&self, options: &SkillLookupOptions) -> Result<SkillProviderOutput, String> {
        let roots = resolve_roots(&self.resolved, options.cwd.as_deref());
        let complete = self.watch.observe_roots(&roots);
        let mut candidates = Vec::new();
        for root in &roots {
            candidates.extend(discover_root(root, &self.fs, &self.name)?);
        }
        Ok(if complete {
            SkillProviderOutput::Complete(candidates)
        } else {
            SkillProviderOutput::Observation {
                candidates,
                complete: false,
            }
        })
    }

    fn get(
        &self,
        candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>, String> {
        let SkillLocator::Local { path, directory } = &candidate.locator else {
            return Ok(None);
        };
        let trusted_host = candidate.source == "bundled";
        let Some(parsed) = parse_skill_file(&self.fs, Path::new(path), trusted_host)? else {
            return Ok(None);
        };
        Ok(Some(SkillDefinition {
            path: Some(parsed.path),
            name: parsed.name,
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            invocation: parsed.invocation,
            source: candidate.source.clone(),
            provider: self.name.clone(),
            resource_base: Some(SkillResourceBase::Directory {
                path: directory.clone(),
            }),
            content: parsed.content,
            metadata: parsed.metadata,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One standing policy over one fixed value (the crate test
    /// precedent).
    struct FixedPolicy(Value);

    impl crate::fs_domain::StandingPolicy for FixedPolicy {
        fn resolve(&self, _session: Option<&str>) -> Result<Value, String> {
            Ok(self.0.clone())
        }
    }

    /// One temp directory removed on drop (the crate test precedent).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base =
                std::env::temp_dir().join(format!("fr-skill-fs-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).expect("temp dir");
            Self(base)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Builds one filesystem domain over the temp cwd.
    fn test_domain(cwd: &Path) -> Arc<FsDomain> {
        let config = crate::fs_domain::FsDomainConfig {
            cwd: cwd.display().to_string(),
            diff_basis_max_bytes: crate::fsio::DEFAULT_DIFF_BASIS_MAX_BYTES,
        };
        Arc::new(FsDomain::new(
            config,
            Box::new(FixedPolicy(Value::Null)),
            None,
        ))
    }

    /// The scripted environment (the config-resolution seam).
    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |key: &str| {
            owned
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn config_pins_the_positive_integer_texts() {
        let cases = [
            (
                FileSystemProviderConfig {
                    watch_stability_threshold_ms: 0,
                    ..FileSystemProviderConfig::default()
                },
                "skill-filesystem: watchStabilityThresholdMs must be a positive integer",
            ),
            (
                FileSystemProviderConfig {
                    watch_poll_interval_ms: 0,
                    ..FileSystemProviderConfig::default()
                },
                "skill-filesystem: watchPollIntervalMs must be a positive integer",
            ),
            (
                FileSystemProviderConfig {
                    watch_max_projects: 0,
                    ..FileSystemProviderConfig::default()
                },
                "skill-filesystem: watchMaxProjects must be a positive integer",
            ),
        ];
        for (config, expected) in cases {
            let error = resolve_config(&config, &env_of(&[])).expect_err("refused");
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn roots_pin_the_rank_order_and_names() {
        let home = TempDir::new("roots-home");
        let project = TempDir::new("roots-project");
        std::fs::create_dir(project.0.join(".git")).expect("git anchor");
        let config = FileSystemProviderConfig {
            fr_home: Some(home.0.display().to_string()),
            agents_home: Some(home.0.join("agents").display().to_string()),
            custom_skill_dirs: vec!["/custom/skills".to_owned()],
            bundled_skill_dir: Some("/bundled/skills".to_owned()),
            ..FileSystemProviderConfig::default()
        };
        let resolved = resolve_config(&config, &env_of(&[])).expect("resolved");
        let roots = resolve_roots(&resolved, Some(project.0.display().to_string()).as_deref());
        let faces: Vec<(&str, i64, bool, bool)> = roots
            .iter()
            .map(|root| (root.source, root.rank, root.skip_system, root.trusted_host))
            .collect();
        assert_eq!(
            faces,
            [
                ("project-fr", PROJECT_FR_RANK, false, false),
                ("project-agents", PROJECT_AGENTS_RANK, false, false),
                ("custom", CUSTOM_RANK, false, false),
                ("user-fr", USER_FR_RANK, true, false),
                ("user-agents", USER_AGENTS_RANK, false, false),
                ("bundled", BUNDLED_SKILL_RANK, false, true),
            ]
        );
        assert_eq!(
            roots[0].path,
            project.0.join(".fr/skills"),
            "the project root carries the fr-brand directory"
        );
        assert_eq!(
            roots[1].path,
            project.0.join(".agents/skills"),
            "the agents directory stays verbatim"
        );
        assert_eq!(roots[0].project_root, Some(project.0.clone()));
    }

    #[test]
    fn roots_drop_the_default_roots_when_isolated() {
        let home = TempDir::new("roots-isolated");
        let config = FileSystemProviderConfig {
            fr_home: Some(home.0.display().to_string()),
            agents_home: Some(home.0.join("agents").display().to_string()),
            include_default_roots: false,
            ..FileSystemProviderConfig::default()
        };
        let env = env_of(&[("FR_BUNDLED_SKILL_DIR", "/env/bundled")]);
        let resolved = resolve_config(&config, &env).expect("resolved");
        assert!(
            resolved.bundled_skill_dir.is_none(),
            "the env bundled root is a default root and an isolated provider must not see it"
        );
        let roots = resolve_roots(&resolved, Some("/somewhere".to_owned()).as_deref());
        assert!(
            roots.is_empty(),
            "an isolated provider sees only its explicit roots"
        );
    }

    #[test]
    fn frontmatter_pins_the_split_arms() {
        let valid = parse_frontmatter("---\nname: a\n---\nbody\n").expect("valid");
        assert_eq!(valid.expect("sections").1, "body\n");
        assert!(
            parse_frontmatter("no fences\n")
                .expect("no error")
                .is_none(),
            "a missing opener is the missing shape"
        );
        assert!(
            parse_frontmatter("---\nname: a\n")
                .expect("no error")
                .is_none(),
            "a missing closer is the missing shape"
        );
        assert!(
            parse_frontmatter("---\n- list\n---\nbody")
                .expect("no error")
                .is_none(),
            "a non-mapping document is the missing shape"
        );
        parse_frontmatter("---\n: :broken:\n---\nbody").unwrap_err();
    }

    #[test]
    fn frontmatter_boolean_pins_the_value_family() {
        let mapping = |text: &str| -> serde_yaml_ng::Mapping {
            let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).expect("yaml");
            value.as_mapping().expect("mapping").clone()
        };
        for (text, expected) in [
            ("flag: true\n", Some(true)),
            ("flag: 1\n", Some(true)),
            ("flag: \"1\"\n", Some(true)),
            ("flag: yes\n", Some(true)),
            ("flag: ON\n", Some(true)),
            ("flag: false\n", Some(false)),
            ("flag: 0\n", Some(false)),
            ("flag: \"0\"\n", Some(false)),
            ("flag: no\n", Some(false)),
            ("flag: Off\n", Some(false)),
        ] {
            assert_eq!(
                frontmatter_boolean(&mapping(text), "flag").expect("parsed"),
                expected,
                "{text}"
            );
        }
        assert_eq!(
            frontmatter_boolean(&mapping("other: 1\n"), "flag").expect("absent"),
            None
        );
        let error = frontmatter_boolean(&mapping("flag: maybe\n"), "flag").expect_err("refused");
        assert_eq!(error, "frontmatter field \"flag\" must be a boolean");
    }

    #[test]
    fn invocation_policy_pins_the_legacy_refusals_and_defaults() {
        let mapping = |text: &str| -> serde_yaml_ng::Mapping {
            let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).expect("yaml");
            value.as_mapping().expect("mapping").clone()
        };
        for (legacy, canonical) in [
            ("disableModelInvocation", "disable-model-invocation"),
            ("modelInvocable", "disable-model-invocation"),
            ("userInvocable", "user-invocable"),
        ] {
            let error = parse_invocation_policy(&mapping(&format!("{legacy}: true\n")))
                .expect_err("legacy");
            assert_eq!(
                error,
                format!("frontmatter field \"{legacy}\" is unsupported; use \"{canonical}\"")
            );
        }
        let defaults = parse_invocation_policy(&mapping("name: a\n")).expect("defaults");
        assert!(defaults.model_invocable && defaults.user_invocable);
        let disabled = parse_invocation_policy(&mapping(
            "disable-model-invocation: true\nuser-invocable: false\n",
        ))
        .expect("disabled");
        assert!(!disabled.model_invocable && !disabled.user_invocable);
    }

    /// Plants the workspace fixture: one project skill bundle, one
    /// flat skill, one ignored entry, one hollow bundle, and one user
    /// agents-home skill.
    fn plant_workspace(project: &Path, agents_home: &Path, bundled: &Path) {
        let skills = project.join(".fr/skills");
        std::fs::create_dir_all(skills.join("alpha")).expect("alpha");
        std::fs::write(
            skills.join("alpha/SKILL.md"),
            "---\nname: alpha\ndescription: alpha skill\n---\nalpha body\n",
        )
        .expect("alpha file");
        std::fs::write(
            skills.join("beta.md"),
            "---\nname: beta\ndescription: beta skill\n---\nbeta body\n",
        )
        .expect("beta file");
        std::fs::write(skills.join("ignored.txt"), "nope").expect("ignored");
        std::fs::create_dir_all(skills.join("hollow")).expect("hollow");
        let user_skills = agents_home.join("skills/gamma");
        std::fs::create_dir_all(&user_skills).expect("gamma");
        std::fs::write(
            user_skills.join("SKILL.md"),
            "---\nname: gamma\ndescription: gamma skill\n---\n",
        )
        .expect("gamma file");
        let bundled_skill = bundled.join("delta");
        std::fs::create_dir_all(&bundled_skill).expect("delta");
        std::fs::write(
            bundled_skill.join("SKILL.md"),
            "---\nname: delta\ndescription: delta skill\n---\ndelta body\n",
        )
        .expect("delta file");
    }

    #[test]
    fn discovery_pins_directory_flat_and_skip_arms() {
        let project = TempDir::new("discover-project");
        let home = TempDir::new("discover-home");
        std::fs::create_dir(project.0.join(".git")).expect("git anchor");
        let bundled = TempDir::new("discover-bundled");
        plant_workspace(&project.0, &home.0.join("agents"), &bundled.0);
        let config = FileSystemProviderConfig {
            fr_home: Some(home.0.display().to_string()),
            agents_home: Some(home.0.join("agents").display().to_string()),
            bundled_skill_dir: Some(bundled.0.display().to_string()),
            ..FileSystemProviderConfig::default()
        };
        let resolved = resolve_config(&config, &env_of(&[])).expect("resolved");
        let domain = test_domain(&project.0);
        let roots = resolve_roots(&resolved, Some(project.0.display().to_string()).as_deref());
        let mut candidates = Vec::new();
        for root in &roots {
            candidates.extend(discover_root(root, &domain, "filesystem").expect("discover"));
        }
        let faces: Vec<(String, i64, String)> = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.name.clone(),
                    candidate.rank,
                    candidate.source.clone(),
                )
            })
            .collect();
        assert_eq!(
            faces,
            [
                ("alpha".to_owned(), PROJECT_FR_RANK, "project-fr".to_owned()),
                ("beta".to_owned(), PROJECT_FR_RANK, "project-fr".to_owned()),
                (
                    "gamma".to_owned(),
                    USER_AGENTS_RANK,
                    "user-agents".to_owned()
                ),
                ("delta".to_owned(), BUNDLED_SKILL_RANK, "bundled".to_owned()),
            ],
            "the ignored entry, the hollow bundle, and the absent roots contribute nothing"
        );
        let alpha = &candidates[0];
        assert_eq!(
            alpha.resource_base,
            Some(SkillResourceBase::Directory {
                path: project.0.join(".fr/skills/alpha").display().to_string(),
            })
        );
        assert_eq!(alpha.metadata, None);
    }

    #[test]
    fn provider_list_and_get_pin_the_end_to_end_shapes() {
        let project = TempDir::new("provider-project");
        let home = TempDir::new("provider-home");
        std::fs::create_dir(project.0.join(".git")).expect("git anchor");
        let bundled = TempDir::new("provider-bundled");
        plant_workspace(&project.0, &home.0.join("agents"), &bundled.0);
        let config = FileSystemProviderConfig {
            fr_home: Some(home.0.display().to_string()),
            agents_home: Some(home.0.join("agents").display().to_string()),
            bundled_skill_dir: Some(bundled.0.display().to_string()),
            ..FileSystemProviderConfig::default()
        };
        let provider = FileSystemSkillProvider::new(
            Weak::new(),
            test_domain(&project.0),
            &config,
            &env_of(&[]),
        )
        .expect("provider");
        let output = provider
            .list(&SkillLookupOptions {
                cwd: Some(project.0.display().to_string()),
            })
            .expect("list");
        let SkillProviderOutput::Complete(candidates) = output else {
            panic!("a healthy watch observes complete discovery");
        };
        assert_eq!(candidates.len(), 4);
        let alpha = candidates
            .iter()
            .find(|candidate| candidate.name == "alpha")
            .expect("alpha");
        let definition = provider
            .get(alpha, &SkillLookupOptions { cwd: None })
            .expect("get")
            .expect("definition");
        assert_eq!(definition.content, "alpha body");
        assert_eq!(definition.provider, "filesystem");
        assert_eq!(definition.source, "project-fr");
        assert_eq!(
            definition.resource_base,
            Some(SkillResourceBase::Directory {
                path: project.0.join(".fr/skills/alpha").display().to_string(),
            })
        );
        let gamma = candidates
            .iter()
            .find(|candidate| candidate.name == "gamma")
            .expect("gamma");
        let loaded = provider
            .get(gamma, &SkillLookupOptions { cwd: None })
            .expect("get")
            .expect("definition");
        assert_eq!(loaded.content, "", "the empty body trims to empty");
    }

    /// One bare watch state (no poll thread; the tests drive ticks at
    /// synthetic instants).
    fn bare_watch(threshold_ms: u64, max_projects: usize) -> Arc<WatchShared> {
        Arc::new(WatchShared {
            invalidator: Weak::new(),
            state: Mutex::new(WatchState::default()),
            stop: AtomicBool::new(false),
            enabled: true,
            stability_threshold_ms: threshold_ms,
            max_projects,
        })
    }

    fn project_root_for(tag: &str) -> TempDir {
        let project = TempDir::new(tag);
        std::fs::create_dir(project.0.join(".git")).expect("git anchor");
        project
    }

    fn config_for(home: &Path, agents: &Path, max_projects: usize) -> FileSystemProviderConfig {
        FileSystemProviderConfig {
            fr_home: Some(home.display().to_string()),
            agents_home: Some(agents.display().to_string()),
            watch_max_projects: max_projects,
            ..FileSystemProviderConfig::default()
        }
    }

    #[test]
    fn watch_pins_the_stability_ladder() {
        let project = project_root_for("watch-stability");
        let home = TempDir::new("watch-stability-home");
        let config = config_for(&home.0, &home.0.join("agents"), 4);
        let resolved = resolve_config(&config, &env_of(&[])).expect("resolved");
        let watch = bare_watch(200, 4);
        let roots = resolve_roots(&resolved, Some(project.0.display().to_string()).as_deref());
        let project_root_path = roots[0].path.clone();
        std::fs::create_dir_all(&project_root_path).expect("skills dir");
        watch.observe_roots(&roots);
        let t0 = Instant::now();
        assert!(
            !watch.tick(t0),
            "the baseline adopts without an invalidation"
        );
        std::fs::write(
            project_root_path.join("beta.md"),
            "---\nname: beta\ndescription: beta skill\n---\n",
        )
        .expect("skill file");
        assert!(
            !watch.tick(t0 + std::time::Duration::from_millis(50)),
            "an unstable change pends"
        );
        assert!(
            !watch.tick(t0 + std::time::Duration::from_millis(150)),
            "the pending window is not elapsed yet"
        );
        assert!(
            watch.tick(t0 + std::time::Duration::from_millis(250)),
            "the change stabilized past the threshold invalidates"
        );
        assert!(
            !watch.tick(t0 + std::time::Duration::from_millis(300)),
            "the adopted signature is stable"
        );
        let state = watch.state.lock().expect("watch state");
        assert!(state.pending.is_empty());
        assert_eq!(state.signatures.len(), roots.len());
    }

    #[test]
    fn watch_pins_the_project_eviction() {
        let home = TempDir::new("watch-evict-home");
        let config = config_for(&home.0, &home.0.join("agents"), 1);
        let resolved = resolve_config(&config, &env_of(&[])).expect("resolved");
        let watch = bare_watch(200, 1);
        let first = project_root_for("watch-evict-a");
        let second = project_root_for("watch-evict-b");
        let first_roots = resolve_roots(&resolved, Some(first.0.display().to_string()).as_deref());
        watch.observe_roots(&first_roots);
        {
            let state = watch.state.lock().expect("watch state");
            assert_eq!(state.projects.len(), 1);
            assert_eq!(
                state.roots.len(),
                4,
                "the two project roots plus the two shared user roots"
            );
        }
        let second_roots =
            resolve_roots(&resolved, Some(second.0.display().to_string()).as_deref());
        watch.observe_roots(&second_roots);
        let (oldest_project, evicted_fully) = {
            let state = watch.state.lock().expect("watch state");
            (
                state.projects[0].0.clone(),
                state
                    .roots
                    .iter()
                    .all(|root| !root.path.starts_with(&first.0)),
            )
        };
        assert_eq!(
            oldest_project, second.0,
            "the oldest project is evicted over the cap"
        );
        assert!(
            evicted_fully,
            "the evicted project's roots drop from the watched set"
        );
    }

    #[test]
    fn watch_pins_the_shared_root_accumulation() {
        let home = TempDir::new("watch-shared-home");
        let mut config = config_for(&home.0, &home.0.join("agents"), 128);
        config.custom_skill_dirs = vec!["/custom/a".to_owned(), "/custom/b".to_owned()];
        let resolved = resolve_config(&config, &env_of(&[])).expect("resolved");
        let watch = bare_watch(200, 128);
        let roots = resolve_roots(&resolved, None);
        watch.observe_roots(&roots);
        let state = watch.state.lock().expect("watch state");
        assert!(state.projects.is_empty(), "shared roots carry no project");
        assert_eq!(
            state.roots.len(),
            4,
            "both custom roots and both default user roots accumulate"
        );
        // A second observation without a cwd keeps the shared roots.
        drop(state);
        watch.observe_roots(&roots);
        let state = watch.state.lock().expect("watch state");
        assert_eq!(state.roots.len(), 4);
    }

    #[test]
    fn watch_pins_the_host_mutation_gate() {
        let project = project_root_for("watch-mutation");
        let home = TempDir::new("watch-mutation-home");
        let config = config_for(&home.0, &home.0.join("agents"), 4);
        let resolved = resolve_config(&config, &env_of(&[])).expect("resolved");
        let watch = bare_watch(200, 4);
        let roots = resolve_roots(&resolved, Some(project.0.display().to_string()).as_deref());
        let skills = roots[0].path.clone();
        std::fs::create_dir_all(&skills).expect("skills dir");
        watch.observe_roots(&roots);
        assert!(
            watch.is_watched_mutation(&skills.join("bundle/SKILL.md")),
            "a nested SKILL.md is a potential skill path"
        );
        assert!(
            watch.is_watched_mutation(&skills.join("flat.md")),
            "a flat markdown entry is a potential skill path"
        );
        assert!(
            !watch.is_watched_mutation(&skills.join("bundle/other.txt")),
            "a deep non-skill file is not a mutation target"
        );
        assert!(
            !watch.is_watched_mutation(&project.0.join("elsewhere.md")),
            "a path outside the watched roots is not a mutation target"
        );
        let user_fr_roots: Vec<&SkillRoot> = roots
            .iter()
            .filter(|root| root.source == "user-fr")
            .collect();
        let user_fr_path = user_fr_roots[0].path.join(".system/hidden.md");
        assert!(
            !user_fr_roots.is_empty() && !is_potential_skill_path(user_fr_roots[0], &user_fr_path),
            "the skip-system root ignores .system paths"
        );
    }
}
