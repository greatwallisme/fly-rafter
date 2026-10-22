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

use std::fmt::Write as _;
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

/// Profile root file name (brand mapping of dsh `PROFILE_ROOT_FILENAME`,
/// profile-boot.ts:88; 2026-10-22 Q23A).
///
/// The root stays an empty entry list: the whole assembly is composed as
/// patch layers, so the root exists only to give boot and the dump the same
/// readable base anchor — dsh writes it on every prepare for the same
/// reason, and its loader include-root role does not exist in fr (the
/// dougong loader takes a whole JSON document).
pub const ROOT_FILENAME: &str = "fr.yml";

/// Root file content rewritten on every prepare (dsh `PROFILE_ROOT_CONFIG`
/// shape, profile-boot.ts:88-93; Q23A). Comment lines are guidance only; the
/// `[]` entry list is the parsed payload.
pub const ROOT_CONFIG: &str = "# 本 profile 的根文件：一份空 entry list。装配树全为补丁层组合：\n# 各 bundle 层，然后本 profile 的 fr.patch.yml，然后 --patch overlays。\n# 请编辑 fr.patch.yml，而非本文件。\n[]\n";

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

impl EntryKind {
    /// The built-in bundle name mounted as layer 1 for this entry; the dump
    /// labels its layer with the same name.
    #[must_use]
    pub const fn bundle_name(self) -> &'static str {
        match self {
            Self::Headless => "headless",
        }
    }
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
    /// Built-in bundle names backing [`Self::layers`], in the same order; the
    /// dump re-parses these raw (shape-only, `!!js` preserved) because the
    /// boot-grade parse evaluates expressions and drops tags.
    pub bundle_names: Vec<String>,
    /// The profile's own patch layer (empty when the file is absent or the
    /// load skipped the user layer).
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

/// Anchors insert-row names that are explicit relative paths (`./` or `../`
/// prefixes) to `base`, mirroring dsh `anchorInsertedPluginNames`
/// (app-boot index.ts:550-561; 2026-10-22 Q6 ruling). Override rows are
/// matchers and are never touched; names without the explicit prefix stay
/// literal (PRD 6.2).
fn anchor_insert_names(lines: &mut [PatchLine], base: &Path) {
    for line in lines {
        let Some(inserted) = &mut line.insert else {
            continue;
        };
        for row in inserted {
            if let Some(name) = &mut row.name {
                if let Some(rest) = name.strip_prefix("./") {
                    *name = join_lexical(base, Path::new(rest)).display().to_string();
                } else if name.starts_with("../") {
                    *name = join_lexical(base, Path::new(name)).display().to_string();
                }
            }
        }
    }
}

/// Lexically joins `base` and `rel`, collapsing `.` and `..` without touching
/// the filesystem (same shape as dsh's `path.resolve`).
pub(crate) fn join_lexical(base: &Path, rel: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = base
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .map(|component| component.as_os_str().to_owned())
        .collect();
    for component in rel.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other.as_os_str().to_owned()),
        }
    }
    parts.into_iter().collect()
}

/// The directory a patch file's relative insert names anchor to: the file's
/// parent, made absolute against the cwd when relative (dsh
/// `dirname(resolve(file))`).
pub(crate) fn anchor_base(path: &Path) -> PathBuf {
    let dir = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    if dir.is_absolute() {
        return dir;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(dir),
        Err(_) => dir,
    }
}

/// Loads an optional patch file: absent = `None` (empty layer), any other
/// read failure or a parse error is fatal (dsh `loadOptionalPatches`).
/// Insert-row relative names anchor to the file's directory (Q6).
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
    let mut lines = parse_patch_lines(&content, ctx)?;
    anchor_insert_names(&mut lines, &anchor_base(path));
    Ok(Some(lines))
}

/// Loads a required `--patch` overlay (dsh `loadOverlayPatches`).
///
/// The caller named this file, so its absence is a misconfiguration and fails
/// startup. Insert-row relative names anchor to the file's directory (Q6).
///
/// # Errors
///
/// [`Error::Io`] when the file cannot be read (including absence); parse
/// errors propagate from [`parse_patch_lines`].
pub fn load_required_overlay(path: &Path, ctx: &EvalContext) -> Result<Vec<PatchLine>> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::Io(format!("failed to read overlay {}: {e}", path.display())))?;
    let mut lines = parse_patch_lines(&content, ctx)?;
    anchor_insert_names(&mut lines, &anchor_base(path));
    Ok(lines)
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
/// non-fatally; the profile's own patch file parses to the `patch` layer
/// unless `user_layer` is false (dsh `prepareProfile`'s `{ userLayer }`
/// option: the default dump skips the user layer entirely, never reading or
/// parsing `fr.patch.yml`); manifest errors are fatal.
///
/// # Errors
///
/// [`Error::Profile`] for illegal names, unknown profiles, and manifest
/// violations; [`Error::Io`] for read failures; patch parse errors propagate.
pub fn load_profile(
    home: &Path,
    name: &str,
    user_layer: bool,
    ctx: &EvalContext,
) -> Result<LoadedProfile> {
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
    let mut bundle_names = Vec::new();
    let mut skipped_bundles = Vec::new();
    for bundle in bundles {
        match builtin_bundle_yaml(&bundle) {
            Some(yaml) => {
                layers.push(parse_patch_lines(yaml, ctx)?);
                bundle_names.push(bundle);
            }
            None => skipped_bundles.push(SkippedBundle {
                name: bundle,
                reason: "not a built-in bundle name".to_owned(),
            }),
        }
    }
    let patch = if user_layer {
        load_optional_patches(&dir.join(PATCH_FILENAME), ctx)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok(LoadedProfile {
        name: name.to_owned(),
        dir,
        layers,
        bundle_names,
        patch,
        skipped_bundles,
    })
}

/// Prepares a profile for boot or dump (dsh `prepareProfile`,
/// profile-boot.ts:194-201; 2026-10-22 Q23A/Q20A).
///
/// A `from_default_profile` template first replaces the existing profile
/// directory (staging rename dance, dsh `initializeProfileFromDefault`); the
/// profile then loads with or without its user patch layer; finally the root
/// file ([`ROOT_FILENAME`]) is rewritten unconditionally so boot and the
/// dump anchor on the same readable base.
///
/// # Errors
///
/// Propagates [`load_profile`] errors; unknown replacement templates and
/// shipped-name targets are rejected (see
/// [`initialize_profile_from_default`]); root write failures are
/// [`Error::Io`].
pub fn prepare_profile(
    home: &Path,
    name: &str,
    user_layer: bool,
    from_default_profile: Option<&str>,
    warn: &dyn Fn(&str),
    ctx: &EvalContext,
) -> Result<LoadedProfile> {
    if let Some(template) = from_default_profile {
        initialize_profile_from_default(home, name, template, warn)?;
    }
    let loaded = load_profile(home, name, user_layer, ctx)?;
    let root = loaded.dir.join(ROOT_FILENAME);
    fs::write(&root, ROOT_CONFIG)
        .map_err(|e| Error::Io(format!("failed to write {}: {e}", root.display())))?;
    Ok(loaded)
}

/// Shipped (built-in) profile names — the entry kinds. Replacement targets
/// colliding with these (case-insensitively) are rejected; kept in sync with
/// [`profile_template_bundles`], which a unit test asserts below.
const SHIPPED_PROFILE_NAMES: &[&str] = &["headless"];

/// Quotes a string the way dsh embeds it in diagnostics (`JSON.stringify`),
/// so the user-visible text matches byte for byte. Hand-rolled because
/// `serde_json::to_string` cannot fail on `&str` yet still returns `Result`.
pub(crate) fn json_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control.is_control() => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            plain => out.push(plain),
        }
    }
    out.push('"');
    out
}

/// Replaces the named custom profile with a fresh copy of one shipped
/// template (dsh `initializeProfileFromDefault`, profile-boot.ts:102-165;
/// Q20A/Q23A).
///
/// Only the template's bundle list is copied; local state from a same-named
/// profile is not read. The new profile assembles in a `.<name>.staging-*`
/// directory beside the profiles directory and moves into place; only then
/// is the previous directory removed. A failed move restores the previous
/// directory; when restoring also fails, the error names the staging path
/// holding it.
///
/// # Errors
///
/// [`Error::Profile`] for an unknown template, a shipped-name target
/// (compared without case), or a concurrent recreation; [`Error::Io`] for
/// file-system failures.
pub fn initialize_profile_from_default(
    home: &Path,
    name: &str,
    from_default_profile: &str,
    warn: &dyn Fn(&str),
) -> Result<()> {
    validate_profile_name(name)?;
    let Some(template_bundles) = profile_template_bundles(from_default_profile) else {
        // dsh sorts the expected keys; the const is kept sorted.
        let expected = SHIPPED_PROFILE_NAMES
            .iter()
            .map(|value| json_quote(value))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::Profile(format!(
            "fr: unknown default profile {}; expected one of {expected}",
            json_quote(from_default_profile)
        )));
    };
    // Case-insensitive file systems resolve a case variant such as `Headless`
    // to the shipped directory, so the target check ignores case too.
    if let Some(shipped) = SHIPPED_PROFILE_NAMES
        .iter()
        .find(|shipped| shipped.eq_ignore_ascii_case(name))
    {
        return Err(Error::Profile(format!(
            "fr: profile {} names shipped profile {} and cannot be a custom profile target; \
             omit --from-default-profile to use it",
            json_quote(name),
            json_quote(shipped)
        )));
    }
    let profiles_dir = home.join(PROFILES_DIR);
    let dir = profiles_dir.join(name);
    // The staging directory sits beside the target so both renames stay on
    // one filesystem; pid plus wall-clock nanos stand in for mkdtemp's random
    // suffix (uniqueness for concurrent replaces is best-effort, matching the
    // dsh concurrency caveat).
    let staging = profiles_dir.join(format!(
        ".{name}.staging-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or_else(|_| 0, |d| d.as_nanos()),
    ));
    fs::create_dir_all(&profiles_dir)
        .map_err(|e| Error::Io(format!("failed to create {}: {e}", profiles_dir.display())))?;
    let fresh = staging.join("new").join(name);
    init_profile(&fresh, template_bundles)?;
    let previous = staging.join("previous");
    let moved = match fs::rename(&dir, &previous) {
        Ok(()) => true,
        // An absent target is a fresh create, not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(Error::Io(format!(
                "failed to move {} to {}: {e}",
                dir.display(),
                previous.display()
            )));
        }
    };
    if let Err(error) = fs::rename(&fresh, &dir) {
        if dir.exists() {
            // Windows reports EPERM instead of EEXIST here, so the target's
            // state identifies a concurrent winner (dsh comment).
            let _ = fs::remove_dir_all(&previous);
            return Err(Error::Profile(format!(
                "fr: profile {} was recreated concurrently at {}",
                json_quote(name),
                dir.display()
            )));
        }
        if moved && let Err(restore_error) = fs::rename(&previous, &dir) {
            return Err(Error::Io(format!(
                "fr: profile replacement failed and {} could not be restored from {}: \
                 move error {error}; restore error {restore_error}",
                dir.display(),
                previous.display()
            )));
        }
        return Err(Error::Io(format!(
            "failed to move {} to {}: {error}",
            fresh.display(),
            dir.display()
        )));
    }
    if let Err(error) = fs::remove_dir_all(&staging) {
        // The replacement is complete; leftover files only occupy space.
        warn(&format!(
            "fr: replaced profile {} but could not remove {}: {error}",
            json_quote(name),
            staging.display()
        ));
    }
    Ok(())
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
/// A selected profile goes through [`prepare_profile`], so boot and the dump
/// anchor on the same freshly rewritten root file (Q23A) and a
/// `from_default_profile` template replaces the profile before loading
/// (Q20A, shared with the dump).
///
/// # Errors
///
/// Fails on illegal profile names, unknown profiles, replacement template
/// errors, manifest errors, unreadable files, required overlay absence, and
/// patch parse errors — parse and required-file failures are fail-closed
/// (startup dies).
pub fn load_stack(
    home: &Path,
    entry: EntryKind,
    profile: Option<&str>,
    overlays: &[PathBuf],
    from_default_profile: Option<&str>,
    warn: &dyn Fn(&str),
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
            let loaded = prepare_profile(home, name, true, from_default_profile, warn, ctx)?;
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
        fs::write(&manifest, "bundles: [a]\nbundles: [b]\n")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        assert!(
            read_profile_manifest(&dir).is_err(),
            "duplicate keys must be rejected, not silently last-wins"
        );
    }

    #[test]
    fn invalid_name_leaves_no_filesystem_side_effects() {
        let home = TempHome::new("no-side-effect");
        for bad in ["", "a/b", ".", "node_modules"] {
            assert!(
                validate_profile_name(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        assert!(
            !home.path().join(PROFILES_DIR).exists(),
            "rejected names must not touch the filesystem"
        );
    }

    #[test]
    fn unknown_manifest_bundle_is_skipped_non_fatal() {
        let home = TempHome::new("skip");
        let dir = home.path().join(PROFILES_DIR).join("work");
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        fs::write(dir.join(MANIFEST_FILENAME), "bundles: [headless, nope]\n")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        let loaded = load_profile(home.path(), "work", true, &EvalContext::neutral())
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
        let loaded = load_profile(home.path(), "headless", true, &EvalContext::neutral())
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
        let outcome = load_profile(home.path(), "ghost", true, &EvalContext::neutral());
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
            None,
            &|_| {},
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
            None,
            &|_| {},
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
        let compiled = crate::patch::compile_to_loader_doc(
            &composed.rows,
            &resolver,
            &|name: &str| name.starts_with("fr-"),
            &|_| None,
        )
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
        // The bundle's !!js values must arrive evaluated (tag-loss regression
        // guard): approval policy and sandbox mode under the neutral context.
        let by_name = |id: &str| {
            plugins
                .iter()
                .find(|p| p["name"] == id)
                .unwrap_or_else(|| panic!("row {id} missing"))
        };
        assert_eq!(by_name("approval-policy")["config"]["policy"], "ask");
        assert_eq!(
            by_name("sandbox")["config"]["mode"],
            "workspace-write",
            "neutral env falls back to workspace-write"
        );
        dougong_loader::Config::parse(&compiled.doc)
            .unwrap_or_else(|e| panic!("loader rejected compiled doc: {e}"));
        let _ = fs::remove_dir_all(&artifacts);
    }

    #[test]
    fn relative_insert_names_anchor_to_patch_dir() {
        let home = TempHome::new("anchor");
        let dir = home.path().join("patches");
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        let overlay = dir.join("rel.yml");
        fs::write(
            &overlay,
            "- insert:\n    - id: a\n      name: ./ext/libfoo.so\n    - id: b\n      name: ../sib/bar.wasm\n    - id: c\n      name: plugins/foo.so\n",
        )
        .unwrap_or_else(|e| panic!("write failed: {e}"));
        let lines = load_required_overlay(&overlay, &EvalContext::neutral())
            .unwrap_or_else(|e| panic!("load failed: {e}"));
        let inserted = lines[0].insert.as_ref().expect("insert group");
        assert_eq!(
            inserted[0].name.as_deref(),
            Some(dir.join("ext/libfoo.so").to_str().expect("utf-8 path")),
            "./ anchored to the patch file dir"
        );
        assert_eq!(
            inserted[1].name.as_deref(),
            Some(
                home.path()
                    .join("sib/bar.wasm")
                    .to_str()
                    .expect("utf-8 path")
            ),
            "../ folds up lexically"
        );
        assert_eq!(
            inserted[2].name.as_deref(),
            Some("plugins/foo.so"),
            "no ./ ../ prefix stays literal"
        );
    }

    #[test]
    fn override_row_names_are_never_anchored() {
        let home = TempHome::new("matcher");
        let overlay = home.path().join("patch.yml");
        fs::write(&overlay, "- id: x\n  name: ./matcher\n  disabled: true\n")
            .unwrap_or_else(|e| panic!("write failed: {e}"));
        let lines = load_required_overlay(&overlay, &EvalContext::neutral())
            .unwrap_or_else(|e| panic!("load failed: {e}"));
        assert_eq!(
            lines[0].name.as_deref(),
            Some("./matcher"),
            "override-row name is a matcher and stays untouched"
        );
    }

    #[test]
    fn prepare_profile_rewrites_root_and_user_layer_flag_gates_patch_read() {
        let home = TempHome::new("prepare-root");
        let profile_dir = home.path().join(PROFILES_DIR).join("headless");
        init_profile(&profile_dir, &[]).unwrap_or_else(|e| panic!("init failed: {e}"));
        // A user edit to the root file is reset on every prepare (dsh
        // writes cordis.yml unconditionally; the root is tool-owned).
        fs::write(profile_dir.join(ROOT_FILENAME), "- id: user-edit\n")
            .unwrap_or_else(|e| panic!("write root failed: {e}"));
        // A syntactically broken patch file would be fatal with user_layer,
        // and must be unread with user_layer=false (default-only recovery).
        fs::write(profile_dir.join(PATCH_FILENAME), "- id: a\n  bogus: 1\n")
            .unwrap_or_else(|e| panic!("write patch failed: {e}"));
        let loaded = prepare_profile(
            home.path(),
            "headless",
            false,
            None,
            &|_| {},
            &EvalContext::neutral(),
        )
        .unwrap_or_else(|e| panic!("default-only prepare failed: {e}"));
        let root = fs::read_to_string(profile_dir.join(ROOT_FILENAME))
            .unwrap_or_else(|e| panic!("read root failed: {e}"));
        assert_eq!(root, ROOT_CONFIG, "root reset to the canonical content");
        assert!(
            loaded.patch.is_empty(),
            "user layer skipped: the broken patch file was never parsed"
        );
        // With the user layer on, the same broken file is fail-closed.
        let outcome = prepare_profile(
            home.path(),
            "headless",
            true,
            None,
            &|_| {},
            &EvalContext::neutral(),
        );
        assert!(outcome.is_err(), "broken user patch must fail the load");
    }

    #[test]
    fn from_default_profile_replaces_existing_profile() {
        let home = TempHome::new("from-default");
        let profile_dir = home.path().join(PROFILES_DIR).join("work");
        init_profile(&profile_dir, &[]).unwrap_or_else(|e| panic!("init failed: {e}"));
        fs::write(profile_dir.join(MANIFEST_FILENAME), "bundles: [headless]\n")
            .unwrap_or_else(|e| panic!("write manifest failed: {e}"));
        fs::write(
            profile_dir.join(PATCH_FILENAME),
            "- id: old\n  disabled: true\n",
        )
        .unwrap_or_else(|e| panic!("write patch failed: {e}"));
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        prepare_profile(
            home.path(),
            "work",
            true,
            Some("headless"),
            &|line| {
                warnings
                    .lock()
                    .unwrap_or_else(|e| panic!("poisoned: {e}"))
                    .push(line.to_owned());
            },
            &EvalContext::neutral(),
        )
        .unwrap_or_else(|e| panic!("replace failed: {e}"));
        // The template replaces the profile wholesale: manifest reset to the
        // template bundle list, patch and root reset to their templates.
        let manifest = fs::read_to_string(profile_dir.join(MANIFEST_FILENAME))
            .unwrap_or_else(|e| panic!("read manifest failed: {e}"));
        assert_eq!(
            manifest,
            render_manifest_template(profile_template_bundles("headless").unwrap_or(&[])),
            "manifest reset to the shipped template"
        );
        let patch = fs::read_to_string(profile_dir.join(PATCH_FILENAME))
            .unwrap_or_else(|e| panic!("read patch failed: {e}"));
        assert_eq!(patch, PATCH_TEMPLATE, "patch reset to the blank template");
        let root = fs::read_to_string(profile_dir.join(ROOT_FILENAME))
            .unwrap_or_else(|e| panic!("read root failed: {e}"));
        assert_eq!(root, ROOT_CONFIG, "root written by prepare");
        assert!(
            warnings
                .into_inner()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .is_empty(),
            "no staging-cleanup warnings expected"
        );
        let staging_leftovers =
            fs::read_dir(profile_dir.parent().unwrap_or_else(|| panic!("parent")))
                .unwrap_or_else(|e| panic!("readdir failed: {e}"))
                .filter_map(std::result::Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(".work.staging-"))
                });
        assert!(
            !staging_leftovers,
            "staging directory removed after a successful replace"
        );
    }

    #[test]
    fn from_default_rejects_unknown_template_and_shipped_target() {
        let home = TempHome::new("from-default-guard");
        let unknown = prepare_profile(
            home.path(),
            "work",
            true,
            Some("nope"),
            &|_| {},
            &EvalContext::neutral(),
        );
        let err = unknown.expect_err("unknown template must be rejected");
        assert!(err.to_string().contains("unknown default profile"), "{err}");
        for target in ["headless", "HEADLESS"] {
            let shipped = prepare_profile(
                home.path(),
                target,
                true,
                Some("headless"),
                &|_| {},
                &EvalContext::neutral(),
            );
            let err = shipped.expect_err("shipped target must be rejected");
            assert!(
                err.to_string().contains("names shipped profile"),
                "{target}: {err}"
            );
        }
    }
}
