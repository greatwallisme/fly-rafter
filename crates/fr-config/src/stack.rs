//! Layered file loading stack (PRD 6.1 loading-file semantics; 2026-10-22
//! Q1-Q5 rulings).
//!
//! Assembles the ordered patch layers bottom-up: the built-in entry default
//! bundle, the selected profile's bundle stack and its own patch file, the
//! home patch, and required `--patch` overlays. File semantics mirror dsh's
//! app-boot (profile.ts / profile-context.ts / index.ts): optional patch
//! files are empty layers when absent, overlays are required, parse errors
//! are fail-closed, and known entry names auto-init their profile directory
//! on first use without overwriting existing files.

use std::fs;
use std::path::{Path, PathBuf};

use crate::cond::EvalContext;
use crate::error::{Error, Result};
use crate::patch::{PatchLine, parse_patch_lines};

/// Directory under the fr home holding every profile (dsh `PROFILES_DIR`).
pub const PROFILES_DIR: &str = "profiles";

/// The patch file name shared by the profile patch and the home patch
/// (brand mapping of dsh `PROFILE_PATCH_FILENAME`).
pub const PATCH_FILENAME: &str = "fr.patch.yml";

/// The profile manifest file name. dsh derives profiles from `package.json`
/// and pnpm projects; fr owns a dedicated file because there is no npm layer.
pub const MANIFEST_FILENAME: &str = "profile.yml";

/// Headless entry default bundle (M1 minimal set): embedded YAML text parsed
/// through the same [`crate::patch::parse_patch_lines`] path as user patches
/// (Q4A), a single flat `insert` group with config values aligned to the
/// PRD 6.4 table (Q5B). Tool-level config defaults from PRD 6.4
/// (repeat/glob/task-list/bash-timeout) land with the fr-tools-builtin config
/// schema (Phase 16) and are intentionally absent here.
const HEADLESS_BUNDLE_YAML: &str = include_str!("bundles/headless/fr.patch.yml");

/// M1 entry kinds; later milestones extend this set (PRD 20 M2-M5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Headless one-shot entry (`fr headless`).
    Headless,
}

/// Built-in bundle YAML by name (profile manifest `bundles:` references).
/// M1 ships the headless entry bundle only.
#[must_use]
pub fn builtin_bundle_yaml(name: &str) -> Option<&'static str> {
    match name {
        "headless" => Some(HEADLESS_BUNDLE_YAML),
        _ => None,
    }
}

/// Initial `bundles:` list for a profile auto-initialized on first use.
///
/// fr templates start empty: the entry default bundle is always mounted at
/// layer 1, so a profile re-listing it would double-apply the same rows.
/// dsh's template bundles are dsh's layer-1 equivalent (profile.ts:179-195).
#[must_use]
pub fn profile_template_bundles(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "headless" => Some(&[]),
        _ => None,
    }
}

/// Validates a profile name per the dsh rule (profile.ts:169-176): empty,
/// `/`, `\`, `.`, `..`, and `node_modules` are rejected.
///
/// # Errors
///
/// Returns [`Error::Profile`] when the name is illegal.
pub fn validate_profile_name(name: &str) -> Result<()> {
    let illegal = name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
        || name == "node_modules";
    if illegal {
        return Err(Error::Profile(format!("invalid profile name {name:?}")));
    }
    Ok(())
}

/// One built-in bundle the profile manifest referenced but fr-config could
/// not load (non-fatal; dsh `SkippedBundle` semantics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBundle {
    /// The bundle name from the profile manifest.
    pub name: String,
    /// Human-readable failure reason.
    pub reason: String,
}

/// A loaded profile: resolved bundle layers plus the profile's own patch layer.
#[derive(Debug, Clone)]
pub struct LoadedProfile {
    /// Profile name (its directory basename).
    pub name: String,
    /// Profile directory under `<home>/profiles/`.
    pub dir: PathBuf,
    /// Parsed bundle layers in manifest order.
    pub layers: Vec<Vec<PatchLine>>,
    /// The profile's own patch layer (empty when the file is absent).
    pub patch: Vec<PatchLine>,
    /// Built-in bundle names the manifest referenced but fr-config does not ship.
    pub skipped_bundles: Vec<SkippedBundle>,
}

/// Reads a profile manifest: `bundles` (optional, default empty) listing
/// built-in bundle names; unknown keys are rejected (fail-closed).
///
/// # Errors
///
/// [`Error::Io`] when the file cannot be read; [`Error::Profile`] when the
/// content is not a mapping, a key is not a string, `bundles` is not a list
/// of non-empty strings, or an unknown key is present.
pub fn read_profile_manifest(dir: &Path) -> Result<Vec<String>> {
    let path = dir.join(MANIFEST_FILENAME);
    let content = fs::read_to_string(&path)
        .map_err(|e| Error::Io(format!("failed to read {}: {e}", path.display())))?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&content)
        .map_err(|e| Error::Profile(format!("failed to parse {}: {e}", path.display())))?;
    let mapping = value
        .as_mapping()
        .ok_or_else(|| Error::Profile(format!("{} must be a mapping", path.display())))?;
    let mut bundles = Vec::new();
    for (key, item) in mapping {
        let key_text = key
            .as_str()
            .ok_or_else(|| Error::Profile(format!("{}: non-string key {key:?}", path.display())))?;
        match key_text {
            "bundles" => {
                let seq = item.as_sequence().ok_or_else(|| {
                    Error::Profile(format!(
                        "{}: bundles must be a list of bundle names",
                        path.display()
                    ))
                })?;
                for entry in seq {
                    let name = entry
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .ok_or_else(|| {
                            Error::Profile(format!(
                                "{}: bundles entries must be non-empty strings",
                                path.display()
                            ))
                        })?;
                    bundles.push(name.to_owned());
                }
            }
            other => {
                return Err(Error::Profile(format!(
                    "{}: unknown manifest key `{other}`",
                    path.display()
                )));
            }
        }
    }
    Ok(bundles)
}

/// Loads an optional patch file: absent = `None` (empty layer), any other
/// read failure or a parse error is fatal (dsh `loadOptionalPatches`).
///
/// # Errors
///
/// [`Error::Io`] when the file exists but cannot be read; parse errors
/// propagate from [`parse_patch_lines`].
pub fn load_optional_patches(path: &Path, ctx: &EvalContext) -> Result<Option<Vec<PatchLine>>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(format!("failed to read {}: {e}", path.display()))),
    };
    parse_patch_lines(&content, ctx).map(Some)
}

/// Loads a required `--patch` overlay: the caller named this file, so its
/// absence is a misconfiguration and fails startup (dsh `loadOverlayPatches`).
///
/// # Errors
///
/// [`Error::Io`] when the file cannot be read (including absence); parse
/// errors propagate from [`parse_patch_lines`].
pub fn load_required_overlay(path: &Path, ctx: &EvalContext) -> Result<Vec<PatchLine>> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::Io(format!("failed to read overlay {}: {e}", path.display())))?;
    parse_patch_lines(&content, ctx)
}

/// Profile patch template written when a profile directory is initialized.
const PATCH_TEMPLATE: &str = "# 本 profile 的补丁层，应用于所有 bundle 层之后：\n# 顶层 YAML 数组（id 定向 config 覆盖、disabled、insert 列表；允许 !!js 表达式）。\n[]\n";

/// Initializes a profile directory: creates it plus the manifest and an empty
/// patch template. Existing files are never touched, so re-running is a
/// no-op on an initialized profile (dsh `initProfile`).
///
/// # Errors
///
/// [`Error::Io`] when the directory or either template cannot be written.
pub fn init_profile(dir: &Path, template_bundles: &[&str]) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|e| Error::Io(format!("failed to create {}: {e}", dir.display())))?;
    let manifest_path = dir.join(MANIFEST_FILENAME);
    if !manifest_path.exists() {
        fs::write(&manifest_path, render_manifest_template(template_bundles))
            .map_err(|e| Error::Io(format!("failed to write {}: {e}", manifest_path.display())))?;
    }
    let patch_path = dir.join(PATCH_FILENAME);
    if !patch_path.exists() {
        fs::write(&patch_path, PATCH_TEMPLATE)
            .map_err(|e| Error::Io(format!("failed to write {}: {e}", patch_path.display())))?;
    }
    Ok(())
}

/// Renders the manifest template with the given initial `bundles:` list.
fn render_manifest_template(template_bundles: &[&str]) -> String {
    let mut text = String::from(
        "# bundles: 宿主内置命名 bundle 列表，自底向上叠加于入口默认 bundle 之上。\nbundles:",
    );
    if template_bundles.is_empty() {
        text.push_str(" []\n");
    } else {
        text.push('\n');
        for name in template_bundles {
            text.push_str("  - ");
            text.push_str(name);
            text.push('\n');
        }
    }
    text
}

/// Loads a profile: validates the name and auto-initializes from the template
/// on first use.
///
/// Bundle layers resolve per the manifest; unknown bundle names are skipped
/// non-fatally; the profile's own patch file parses to the `patch` layer;
/// manifest and patch errors are fatal.
///
/// # Errors
///
/// [`Error::Profile`] for illegal names, unknown profiles, and manifest
/// violations; [`Error::Io`] for read failures; patch parse errors propagate.
pub fn load_profile(home: &Path, name: &str, ctx: &EvalContext) -> Result<LoadedProfile> {
    validate_profile_name(name)?;
    let dir = home.join(PROFILES_DIR).join(name);
    let manifest_path = dir.join(MANIFEST_FILENAME);
    if !manifest_path.exists() {
        match profile_template_bundles(name) {
            Some(bundles) => init_profile(&dir, bundles)?,
            None => {
                return Err(Error::Profile(format!("profile {name:?} does not exist")));
            }
        }
    }
    let bundles = read_profile_manifest(&dir)?;
    let mut layers = Vec::new();
    let mut skipped_bundles = Vec::new();
    for bundle in bundles {
        match builtin_bundle_yaml(&bundle) {
            Some(yaml) => layers.push(parse_patch_lines(yaml, ctx)?),
            None => skipped_bundles.push(SkippedBundle {
                name: bundle,
                reason: "not a built-in bundle name".to_owned(),
            }),
        }
    }
    let patch = load_optional_patches(&dir.join(PATCH_FILENAME), ctx)?.unwrap_or_default();
    Ok(LoadedProfile {
        name: name.to_owned(),
        dir,
        layers,
        patch,
        skipped_bundles,
    })
}

/// The ordered layer stack for one launch.
#[derive(Debug, Clone)]
pub struct StackOutcome {
    /// Patch layers in application order (later layers override earlier ones).
    pub layers: Vec<Vec<PatchLine>>,
    /// Non-fatal skips collected while loading profile bundle layers.
    pub skipped_bundles: Vec<SkippedBundle>,
}

/// Assembles the ordered patch layers bottom-up (PRD 6.1): the entry default
/// bundle, then the selected profile (bundle stack + own patch), then the
/// home patch, then required `--patch` overlays.
///
/// # Errors
///
/// Fails on illegal profile names, unknown profiles, manifest errors,
/// unreadable files, required overlay absence, and patch parse errors —
/// parse and required-file failures are fail-closed (startup dies).
pub fn load_stack(
    home: &Path,
    entry: EntryKind,
    profile: Option<&str>,
    overlays: &[PathBuf],
    ctx: &EvalContext,
) -> Result<StackOutcome> {
    let entry_yaml = match entry {
        EntryKind::Headless => HEADLESS_BUNDLE_YAML,
    };
    let mut layers = vec![parse_patch_lines(entry_yaml, ctx)?];
    // `profile_patch` stays `None` without a selected profile so no phantom
    // empty layer is appended.
    let (profile_layers, profile_patch, skipped_bundles) = match profile {
        Some(name) => {
            let loaded = load_profile(home, name, ctx)?;
            (loaded.layers, Some(loaded.patch), loaded.skipped_bundles)
        }
        None => (Vec::new(), None, Vec::new()),
    };
    layers.extend(profile_layers);
    layers.extend(profile_patch);
    if let Some(home_layer) = load_optional_patches(&home.join(PATCH_FILENAME), ctx)? {
        layers.push(home_layer);
    }
    for overlay in overlays {
        layers.push(load_required_overlay(overlay, ctx)?);
    }
    Ok(StackOutcome {
        layers,
        skipped_bundles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::compose;
    use serde_json::Value;

    /// Creates a unique temp directory used as an fr home; removes it on drop.
    struct TempHome {
        dir: PathBuf,
    }

    impl TempHome {
        fn new(label: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("fr-config-stack-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
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

    #[test]
    fn profile_name_validation_rejects_dsh_rules() {
        for bad in ["", "a/b", "a\\b", ".", "..", "node_modules"] {
            assert!(
                validate_profile_name(bad).is_err(),
                "name {bad:?} must be rejected"
            );
        }
        validate_profile_name("work").unwrap_or_else(|e| panic!("valid name rejected: {e}"));
    }

    #[test]
    fn optional_patch_absent_is_empty_layer() {
        let home = TempHome::new("opt");
        let absent = load_optional_patches(&home.path().join("nope.yml"), &EvalContext::neutral())
            .unwrap_or_else(|e| panic!("absent file must be Ok(None): {e}"));
        assert!(absent.is_none());
        let present = home.path().join("patch.yml");
        fs::write(&present, "- id: llm-primary\n  disabled: true\n")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        let layer = load_optional_patches(&present, &EvalContext::neutral())
            .unwrap_or_else(|e| panic!("present file must parse: {e}"));
        assert_eq!(layer.expect("some").len(), 1);
    }

    #[test]
    fn optional_patch_parse_error_is_fatal() {
        let home = TempHome::new("opt-bad");
        let path = home.path().join("bad.yml");
        fs::write(&path, "- id: a\n  bogus: 1\n").unwrap_or_else(|e| panic!("write failed: {e}"));
        assert!(
            load_optional_patches(&path, &EvalContext::neutral()).is_err(),
            "parse errors must fail closed"
        );
    }

    #[test]
    fn required_overlay_absent_fails_startup() {
        let home = TempHome::new("overlay");
        let outcome =
            load_required_overlay(&home.path().join("ghost.yml"), &EvalContext::neutral());
        assert!(outcome.is_err(), "missing overlay must fail");
    }

    #[test]
    fn manifest_schema_is_strict() {
        let home = TempHome::new("manifest");
        let dir = home.path().join(PROFILES_DIR).join("work");
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        let manifest = dir.join(MANIFEST_FILENAME);
        fs::write(&manifest, "bundles: [headless]\n")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        let bundles =
            read_profile_manifest(&dir).unwrap_or_else(|e| panic!("valid manifest failed: {e}"));
        assert_eq!(bundles, vec!["headless".to_owned()]);
        fs::write(&manifest, "bogus: 1\n").unwrap_or_else(|e| panic!("write failed: {e}"));
        assert!(read_profile_manifest(&dir).is_err(), "unknown key rejected");
        fs::write(&manifest, "bundles: [\"\"]\n").unwrap_or_else(|e| panic!("write failed: {e}"));
        assert!(read_profile_manifest(&dir).is_err(), "empty name rejected");
        fs::write(&manifest, "- a\n- b\n").unwrap_or_else(|e| panic!("write failed: {e}"));
        assert!(
            read_profile_manifest(&dir).is_err(),
            "non-mapping manifest rejected"
        );
    }

    #[test]
    fn unknown_manifest_bundle_is_skipped_non_fatal() {
        let home = TempHome::new("skip");
        let dir = home.path().join(PROFILES_DIR).join("work");
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        fs::write(dir.join(MANIFEST_FILENAME), "bundles: [headless, nope]\n")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        let loaded = load_profile(home.path(), "work", &EvalContext::neutral())
            .unwrap_or_else(|e| panic!("load failed: {e}"));
        assert_eq!(loaded.layers.len(), 1, "only the known bundle loads");
        assert_eq!(loaded.skipped_bundles.len(), 1);
        assert_eq!(loaded.skipped_bundles[0].name, "nope");
    }

    #[test]
    fn init_profile_is_idempotent_and_never_overwrites() {
        let home = TempHome::new("init");
        let dir = home.path().join(PROFILES_DIR).join("work");
        init_profile(&dir, &[]).unwrap_or_else(|e| panic!("init failed: {e}"));
        let manifest = dir.join(MANIFEST_FILENAME);
        assert!(manifest.exists() && dir.join(PATCH_FILENAME).exists());
        let custom = "bundles: [headless]\n";
        fs::write(&manifest, custom).unwrap_or_else(|e| panic!("write failed: {e}"));
        init_profile(&dir, &[]).unwrap_or_else(|e| panic!("second init failed: {e}"));
        let content = fs::read_to_string(&manifest).unwrap_or_else(|e| panic!("read failed: {e}"));
        assert_eq!(content, custom, "re-init must not overwrite");
    }

    #[test]
    fn fresh_headless_profile_auto_inits() {
        let home = TempHome::new("auto");
        let loaded = load_profile(home.path(), "headless", &EvalContext::neutral())
            .unwrap_or_else(|e| panic!("auto-init failed: {e}"));
        assert!(
            home.path()
                .join(PROFILES_DIR)
                .join("headless")
                .join(MANIFEST_FILENAME)
                .exists(),
            "manifest must be created"
        );
        assert!(loaded.layers.is_empty(), "template bundles start empty");
        assert!(loaded.patch.is_empty(), "template patch parses to no rows");
        assert!(
            loaded.skipped_bundles.is_empty(),
            "no skips expected: {:?}",
            loaded.skipped_bundles
        );
    }

    #[test]
    fn unknown_profile_name_errors() {
        let home = TempHome::new("ghost");
        let outcome = load_profile(home.path(), "ghost", &EvalContext::neutral());
        assert!(outcome.is_err(), "unknown profile must fail");
    }

    #[test]
    fn stack_layers_ordered_bottom_up() {
        let home = TempHome::new("order");
        // Profile with one override row (auto-init then patch the patch file).
        let profile_dir = home.path().join(PROFILES_DIR).join("headless");
        init_profile(
            &profile_dir,
            profile_template_bundles("headless").unwrap_or(&[]),
        )
        .unwrap_or_else(|e| panic!("init failed: {e}"));
        fs::write(
            profile_dir.join(PATCH_FILENAME),
            "- id: llm-primary\n  config:\n    model: from-profile\n",
        )
        .unwrap_or_else(|e| panic!("write failed: {e}"));
        // Home patch flips the same row.
        fs::write(
            home.path().join(PATCH_FILENAME),
            "- id: llm-primary\n  config:\n    model: from-home\n",
        )
        .unwrap_or_else(|e| panic!("write failed: {e}"));
        // Overlay wins last.
        let overlay = home.path().join("overlay.yml");
        fs::write(
            &overlay,
            "- id: llm-primary\n  config:\n    model: from-overlay\n",
        )
        .unwrap_or_else(|e| panic!("write failed: {e}"));
        let outcome = load_stack(
            home.path(),
            EntryKind::Headless,
            Some("headless"),
            &[overlay],
            &EvalContext::neutral(),
        )
        .unwrap_or_else(|e| panic!("load_stack failed: {e}"));
        // entry + profile bundle layers (none) + profile patch + home + overlay.
        assert_eq!(outcome.layers.len(), 4, "one layer per loaded source");
        assert!(
            outcome.skipped_bundles.is_empty(),
            "no skips expected: {:?}",
            outcome.skipped_bundles
        );
        let composed = compose(&outcome.layers);
        let row = composed
            .rows
            .iter()
            .find(|row| row.id == "llm-primary")
            .unwrap_or_else(|| panic!("llm-primary missing"));
        assert_eq!(row.config, serde_json::json!({"model": "from-overlay"}));
    }

    #[test]
    fn entry_bundle_compiles_to_valid_loader_doc() {
        let outcome = load_stack(
            Path::new("/nonexistent-fr-home"),
            EntryKind::Headless,
            None,
            &[],
            &EvalContext::neutral(),
        )
        .unwrap_or_else(|e| panic!("entry-only stack failed: {e}"));
        assert_eq!(outcome.layers.len(), 1);
        assert!(
            outcome.skipped_bundles.is_empty(),
            "no skips expected: {:?}",
            outcome.skipped_bundles
        );
        let composed = compose(&outcome.layers);
        assert!(composed.warnings.is_empty(), "{:?}", composed.warnings);
        // Materialize an artifact for every first-party row, then compile.
        let artifacts =
            std::env::temp_dir().join(format!("fr-config-stack-entry-{}", std::process::id()));
        fs::create_dir_all(&artifacts).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        for row in &composed.rows {
            let slug = row.name.replace('-', "_");
            fs::write(artifacts.join(format!("lib{slug}.so")), b"")
                .unwrap_or_else(|e| panic!("write {} failed: {e}", row.name));
        }
        let resolver = crate::source::PathResolver::new(vec![artifacts.clone()]);
        let compiled =
            crate::patch::compile_to_loader_doc(&composed.rows, &resolver, &|name: &str| {
                name.starts_with("fr-")
            })
            .unwrap_or_else(|e| panic!("compile failed: {e}"));
        assert!(
            compiled.failures.is_empty(),
            "unexpected failures: {:?}",
            compiled.failures
        );
        let doc: Value =
            serde_json::from_str(&compiled.doc).unwrap_or_else(|e| panic!("bad json: {e}"));
        let plugins = doc["plugins"].as_array().expect("plugins array");
        assert!(!plugins.is_empty(), "M1 bundle must mount plugins");
        dougong_loader::Config::parse(&compiled.doc)
            .unwrap_or_else(|e| panic!("loader rejected compiled doc: {e}"));
        let _ = fs::remove_dir_all(&artifacts);
    }
}
