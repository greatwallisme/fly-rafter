// Rust guideline compliant 2026-10-28
//! Workspace-instruction configuration (dsh agent-instructions/src/config.ts
//! plus the home-path resolution it leans on, verbatim semantics).
//!
//! Brand mapping for the harness home (a runtime directory fact, not
//! extracted prose): `FR_HOME` / `~/.fr` stand in for `DSH_HOME` /
//! `~/.dsh`; the shared agents root keeps `.agents` under
//! `FR_AGENTS_HOME`.

use serde::{Deserialize, Serialize};

/// Directory name for the default Fly Rafter home under the OS home.
pub const FR_HOME_DIR_NAME: &str = ".fr";

/// Stable user-facing display form for the default Fly Rafter home.
pub const DEFAULT_FR_HOME_DISPLAY: &str = "~/.fr";

/// Environment variable that overrides the default Fly Rafter home.
pub const FR_HOME_ENV: &str = "FR_HOME";

/// Directory name for the default shared agent configuration root.
pub const AGENTS_HOME_DIR_NAME: &str = ".agents";

/// Stable user-facing display form for the default shared agents root.
pub const DEFAULT_AGENTS_HOME_DISPLAY: &str = "~/.agents";

/// Environment variable that overrides the default shared agents root.
pub const AGENTS_HOME_ENV: &str = "FR_AGENTS_HOME";

const DEFAULT_PROJECT_ROOT_MARKERS: &[&str] = &[".git"];
const DEFAULT_INSTRUCTION_FILE_CANDIDATES: &[&str] = &["AGENTS.md", "CLAUDE.md"];
const DEFAULT_LOCAL_INSTRUCTION_FILE_CANDIDATES: &[&str] = &["AGENTS.local.md", "CLAUDE.local.md"];
const DEFAULT_MAX_SOURCE_BYTES: u64 = 1_048_576;
const RESERVED_PATH_SEGMENTS: &[&str] = &["", ".", ".."];

/// One environment lookup for home resolution (tests inject maps; the
/// production seam reads the process environment).
pub type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Expand supported tilde prefixes against the operating-system home (dsh
/// `expandHomePath`).
#[must_use]
pub fn expand_home_path(path: &str, home: &str) -> String {
    if path == "~" {
        return home.to_string();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    path.to_string()
}

/// Lexically normalize one path against a working directory (dsh
/// `path.resolve`): absolute paths clean, relative paths join the base,
/// `.` and `..` components fold away.
#[must_use]
pub fn resolve_path(cwd: &str, path: &str) -> String {
    let joined = if path.starts_with('/') || cwd.is_empty() {
        path.to_string()
    } else {
        format!("{cwd}/{path}")
    };
    let mut out: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.pop().is_none() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let folded = out.join("/");
    if joined.starts_with('/') {
        format!("/{folded}")
    } else if folded.is_empty() {
        ".".to_string()
    } else {
        folded
    }
}

/// Resolve one user-data root: an explicit configured path, an
/// environment override (blank counts as unset), then the fallback (dsh
/// `resolveRoot`).
#[must_use]
pub fn resolve_root(
    configured: Option<&str>,
    env: &EnvLookup<'_>,
    env_name: &str,
    home: &str,
    cwd: &str,
    fallback: &str,
) -> String {
    let from_env = env(env_name).filter(|value| !value.trim().is_empty());
    let selected = configured
        .map(str::to_string)
        .or(from_env)
        .unwrap_or_else(|| fallback.to_string());
    resolve_path(cwd, &expand_home_path(&selected, home))
}

/// The default Fly Rafter home (`~/.fr`).
#[must_use]
pub fn default_fr_home(home: &str) -> String {
    format!("{home}/{FR_HOME_DIR_NAME}")
}

/// The default shared agents root (`~/.agents`).
#[must_use]
pub fn default_agents_home(home: &str) -> String {
    format!("{home}/{AGENTS_HOME_DIR_NAME}")
}

/// Label one resolved user-data root symbolically (dsh `rootDisplay`):
/// the default root keeps its `~/` label, anything else shows as
/// `$ENV_NAME`.
#[must_use]
pub fn root_display(
    resolved: &str,
    default_root: &str,
    default_label: &str,
    env_name: &str,
) -> String {
    if resolved == default_root {
        default_label.to_string()
    } else {
        format!("${env_name}")
    }
}

/// User-facing workspace instruction loader configuration (dsh `Config`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionConfig {
    /// Directory entries that identify the project root while walking
    /// upward from the current working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root_markers: Option<Vec<String>>,
    /// UTF-8 byte cap for one rendered baseline or dynamic batch;
    /// non-positive disables loading.
    pub max_bytes: i64,
    /// Maximum UTF-8 bytes read from one instruction file; larger files
    /// are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_source_bytes: Option<u64>,
    /// Ordered same-directory project candidates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_file_candidates: Option<Vec<String>>,
    /// Ordered same-directory local-overlay candidates; empty disables the
    /// overlay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_instruction_file_candidates: Option<Vec<String>>,
}

impl InstructionConfig {
    /// The base-patch defaults (dsh bundle/base `agent-instructions` row).
    #[must_use]
    pub const fn base_patch() -> Self {
        Self {
            project_root_markers: None,
            max_bytes: 65_536,
            max_source_bytes: None,
            instruction_file_candidates: None,
            local_instruction_file_candidates: None,
        }
    }
}

/// Normalized discovery configuration (dsh `ResolvedDiscoveryConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDiscovery {
    /// The resolved Fly Rafter home.
    pub fr_home: String,
    /// The symbolic display label of the Fly Rafter home.
    pub fr_home_display: String,
    /// The resolved shared agents root.
    pub agents_home: String,
    /// The symbolic display label of the shared agents root.
    pub agents_home_display: String,
    /// Project-root marker directory entries.
    pub project_root_markers: Vec<String>,
    /// Ordered same-directory project candidates.
    pub instruction_file_candidates: Vec<String>,
    /// Ordered local-overlay candidates.
    pub local_instruction_file_candidates: Vec<String>,
}

/// Normalized configuration used by discovery and reconciliation (dsh
/// `ResolvedConfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedInstructionConfig {
    /// Discovery roots, markers, and candidates.
    pub discovery: ResolvedDiscovery,
    /// Render byte budget.
    pub max_bytes: i64,
    /// Per-file source cap.
    pub max_source_bytes: u64,
}

/// Resolve homes, root markers, and valid same-directory candidates (dsh
/// `resolveDiscoveryConfig`).
#[must_use]
pub fn resolve_discovery(
    config: &InstructionConfig,
    env: &EnvLookup<'_>,
    home: &str,
    cwd: &str,
) -> ResolvedDiscovery {
    let fr_home = resolve_root(None, env, FR_HOME_ENV, home, cwd, &default_fr_home(home));
    let agents_home = resolve_root(
        None,
        env,
        AGENTS_HOME_ENV,
        home,
        cwd,
        &default_agents_home(home),
    );
    let fr_home_display = root_display(
        &fr_home,
        &default_fr_home(home),
        DEFAULT_FR_HOME_DISPLAY,
        FR_HOME_ENV,
    );
    let agents_home_display = root_display(
        &agents_home,
        &default_agents_home(home),
        DEFAULT_AGENTS_HOME_DISPLAY,
        AGENTS_HOME_ENV,
    );
    ResolvedDiscovery {
        fr_home,
        fr_home_display,
        agents_home,
        agents_home_display,
        project_root_markers: config.project_root_markers.clone().unwrap_or_else(|| {
            DEFAULT_PROJECT_ROOT_MARKERS
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        }),
        instruction_file_candidates: resolve_instruction_file_candidates(
            config.instruction_file_candidates.as_deref(),
            DEFAULT_INSTRUCTION_FILE_CANDIDATES,
        ),
        local_instruction_file_candidates: resolve_instruction_file_candidates(
            config.local_instruction_file_candidates.as_deref(),
            DEFAULT_LOCAL_INSTRUCTION_FILE_CANDIDATES,
        ),
    }
}

/// Resolve the full normalized configuration (dsh `resolveConfig`).
#[must_use]
pub fn resolve_config(
    config: &InstructionConfig,
    env: &EnvLookup<'_>,
    home: &str,
    cwd: &str,
) -> ResolvedInstructionConfig {
    ResolvedInstructionConfig {
        discovery: resolve_discovery(config, env, home, cwd),
        max_bytes: config.max_bytes,
        max_source_bytes: config.max_source_bytes.unwrap_or(DEFAULT_MAX_SOURCE_BYTES),
    }
}

/// Drop candidates that are not same-directory file names (dsh
/// `resolveInstructionFileCandidates`).
fn resolve_instruction_file_candidates(
    candidates: Option<&[String]>,
    fallback: &[&str],
) -> Vec<String> {
    candidates.map_or_else(
        || fallback.iter().map(|s| (*s).to_string()).collect(),
        |list| {
            list.iter()
                .filter(|candidate| {
                    !RESERVED_PATH_SEGMENTS.contains(&candidate.as_str())
                        && !candidate.contains('/')
                        && !candidate.contains('\\')
                })
                .cloned()
                .collect()
        },
    )
}

/// The discovery, precedence, and budget identity of one baseline (dsh
/// `workspaceBaselineIdentity`); the JSON key order matches dsh.
#[must_use]
pub fn workspace_baseline_identity(
    config: &ResolvedInstructionConfig,
    cwd: &str,
    project_root: &str,
) -> String {
    let discovery = &config.discovery;
    serde_json::to_string(&serde_json::json!({
        "projectRoot": relative_path(cwd, project_root),
        "projectRootMarkers": discovery.project_root_markers,
        "maxBytes": config.max_bytes,
        "maxSourceBytes": config.max_source_bytes,
        "instructionFileCandidates": discovery.instruction_file_candidates,
        "localInstructionFileCandidates": discovery.local_instruction_file_candidates,
    }))
    .unwrap_or_default()
}

/// `path.relative(from, to)` for absolute paths (the identity computation
/// only; discovery uses its own helpers).
#[must_use]
pub fn relative_path(from: &str, to: &str) -> String {
    let from_parts: Vec<&str> = from.split('/').filter(|p| !p.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|p| !p.is_empty()).collect();
    let mut shared = 0;
    while shared < from_parts.len()
        && shared < to_parts.len()
        && from_parts[shared] == to_parts[shared]
    {
        shared += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in shared..from_parts.len() {
        parts.push("..".to_string());
    }
    parts.extend(to_parts[shared..].iter().map(|part| (*part).to_string()));
    if parts.is_empty() {
        return String::new();
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn defaults_come_from_the_dsh_tables() {
        assert_eq!(
            DEFAULT_INSTRUCTION_FILE_CANDIDATES,
            ["AGENTS.md", "CLAUDE.md"]
        );
        assert_eq!(
            DEFAULT_LOCAL_INSTRUCTION_FILE_CANDIDATES,
            ["AGENTS.local.md", "CLAUDE.local.md"]
        );
        assert_eq!(DEFAULT_MAX_SOURCE_BYTES, 1_048_576);
        assert_eq!(DEFAULT_PROJECT_ROOT_MARKERS, [".git"]);
    }

    #[test]
    fn base_patch_max_bytes_is_extracted_verbatim() {
        assert_eq!(InstructionConfig::base_patch().max_bytes, 65_536);
        assert_eq!(
            crate::prompts::AGENT_INSTRUCTIONS_MAX_BYTES,
            usize::try_from(65_536).expect("the base-patch budget fits usize")
        );
    }

    #[test]
    fn home_precedence_is_configured_env_then_default() {
        let env = HashMap::from([(FR_HOME_ENV.to_string(), "/env/home".to_string())]);
        let lookup = |name: &str| env.get(name).cloned();
        let resolved = resolve_root(
            Some("/cfg"),
            &lookup,
            FR_HOME_ENV,
            "/home/u",
            "/w",
            "/home/u/.fr",
        );
        assert_eq!(resolved, "/cfg");
        let resolved = resolve_root(None, &lookup, FR_HOME_ENV, "/home/u", "/w", "/home/u/.fr");
        assert_eq!(resolved, "/env/home");
        let blank = HashMap::from([(FR_HOME_ENV.to_string(), "   ".to_string())]);
        let lookup = |name: &str| blank.get(name).cloned();
        let resolved = resolve_root(None, &lookup, FR_HOME_ENV, "/home/u", "/w", "/home/u/.fr");
        assert_eq!(resolved, "/home/u/.fr");
    }

    #[test]
    fn tilde_prefixes_expand_against_the_os_home() {
        assert_eq!(expand_home_path("~", "/home/u"), "/home/u");
        assert_eq!(expand_home_path("~/x", "/home/u"), "/home/u/x");
        assert_eq!(expand_home_path("/abs", "/home/u"), "/abs");
    }

    #[test]
    fn resolve_path_folds_dot_components() {
        assert_eq!(resolve_path("/w", "a/../b"), "/w/b");
        assert_eq!(resolve_path("/w", "/x/./y"), "/x/y");
    }

    #[test]
    fn root_display_labels_the_default_and_the_override() {
        assert_eq!(
            root_display(
                "/home/u/.fr",
                "/home/u/.fr",
                DEFAULT_FR_HOME_DISPLAY,
                FR_HOME_ENV
            ),
            "~/.fr"
        );
        assert_eq!(
            root_display(
                "/opt/fr",
                "/home/u/.fr",
                DEFAULT_FR_HOME_DISPLAY,
                FR_HOME_ENV
            ),
            "$FR_HOME"
        );
    }

    #[test]
    fn candidates_filter_non_file_names() {
        let config = InstructionConfig {
            instruction_file_candidates: Some(vec![
                "AGENTS.md".to_string(),
                ".".to_string(),
                "..".to_string(),
                String::new(),
                "nested/name.md".to_string(),
                "CLAUDE.md".to_string(),
            ]),
            ..InstructionConfig::base_patch()
        };
        let resolved = resolve_discovery(&config, &no_env, "/home/u", "/w");
        assert_eq!(
            resolved.instruction_file_candidates,
            ["AGENTS.md".to_string(), "CLAUDE.md".to_string()]
        );
    }

    #[test]
    fn baseline_identity_serializes_the_dsh_key_order() {
        let config = InstructionConfig::base_patch();
        let resolved = resolve_config(&config, &no_env, "/home/u", "/w/proj");
        let identity = workspace_baseline_identity(&resolved, "/w/proj", "/w/proj");
        let expected = serde_json::to_string(&serde_json::json!({
            "projectRoot": "",
            "projectRootMarkers": [".git"],
            "maxBytes": 65536,
            "maxSourceBytes": 1_048_576,
            "instructionFileCandidates": ["AGENTS.md", "CLAUDE.md"],
            "localInstructionFileCandidates": ["AGENTS.local.md", "CLAUDE.local.md"]
        }))
        .expect("serde json");
        assert_eq!(identity, expected);
    }

    #[test]
    fn relative_path_climbs_and_descends() {
        assert_eq!(relative_path("/w/proj", "/w/proj"), "");
        assert_eq!(relative_path("/w", "/w/proj/sub"), "proj/sub");
        assert_eq!(relative_path("/w/proj/sub", "/w/other"), "../../other");
        assert_eq!(relative_path("/w/proj", "/w/other"), "../other");
    }
}
