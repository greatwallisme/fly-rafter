// Rust guideline compliant 2026-11-01
//! The `load_workspace_dependencies` tool (dsh
//! `tool-workspace-dependencies`): the model-facing query for a
//! bundled Python/Node/pnpm payload, in place or installed under the
//! home.
//!
//! The Q16f-E ruling: the opt-in tool carries a required `source`
//! config (the payload directory) and an optional `root` (the
//! first-call install directory); the baked composition mounts no row
//! and ships no payload — the dsh no-payload deployment is the same
//! shape, so the absence is no deviation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::tools::BodyError;

/// The baked gate: the dsh base bundle mounts no row (the desktop
/// composition only — the Q16f-E baked-off ruling).
pub const BAKED_WORKSPACE_DEPENDENCIES: bool = false;

/// The semver-ish version shape (dsh `VERSION`).
fn is_version(value: &str) -> bool {
    let mut parts = value.split('.');
    let mut numbers = 0;
    while numbers < 3 {
        match parts.next() {
            Some(part) if !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()) => {}
            _ => return false,
        }
        numbers += 1;
    }
    let rest = parts.next();
    match rest {
        None => parts.next().is_none(),
        Some(suffix) => {
            if parts.next().is_some() || suffix.is_empty() {
                return false;
            }
            let head = suffix.chars().next().expect("non-empty checked");
            if head != '-' && head != '+' {
                return false;
            }
            suffix[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
        }
    }
}

/// The distribution-name shape (dsh `/^[A-Za-z0-9][A-Za-z0-9._-]*$/`).
fn is_distribution_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// The distribution-version shape (dsh `/^\d[\w.!+-]*$/`).
fn is_distribution_version(version: &str) -> bool {
    let mut chars = version.chars();
    match chars.next() {
        Some(first) if first.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '!' || c == '+' || c == '-')
}

/// The validated payload metadata (dsh `PrimaryRuntimeManifest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryRuntime {
    /// The bundle build version.
    pub desktop_version: String,
    /// The payload platform word.
    pub platform: String,
    /// The payload architecture word.
    pub arch: String,
    /// The locked payload identity.
    pub payload_digest: Option<String>,
    /// The bundled Python interpreter version.
    pub python: String,
    /// The bundled Node.js version when the payload ships it.
    pub node: Option<String>,
    /// The bundled pnpm version when the payload ships Node too.
    pub pnpm: Option<String>,
    /// The recorded Python distribution versions.
    pub python_packages: BTreeMap<String, String>,
}

/// Validates payload JSON and normalizes the legacy component fields
/// (dsh `parsePrimaryRuntime`).
///
/// # Errors
///
/// The dsh invalid/conflicting refusals.
pub fn parse_primary_runtime(value: &Value) -> Result<PrimaryRuntime, String> {
    let Some(object) = value.as_object() else {
        return Err("primary runtime: invalid metadata".to_owned());
    };
    let legacy = object.contains_key("components");
    let versions = if legacy {
        object.get("components").cloned().unwrap_or(Value::Null)
    } else {
        value.clone()
    };
    let Some(versions) = versions.as_object() else {
        return Err("primary runtime: invalid metadata".to_owned());
    };
    if legacy
        && ["python", "node", "pnpm"]
            .iter()
            .any(|key| object.contains_key(*key))
    {
        return Err("primary runtime: invalid metadata".to_owned());
    }
    let text = |object: &serde_json::Map<String, Value>, key: &str| {
        object.get(key).and_then(Value::as_str).map(str::to_owned)
    };
    let desktop_version = text(object, "desktopVersion");
    let platform = text(object, "platform");
    let arch = text(object, "arch");
    let payload_digest = text(object, "payloadDigest");
    let python = text(versions, "python");
    let node = text(versions, "node");
    let pnpm = text(versions, "pnpm");
    let python_packages = if !object.contains_key("pythonPackages") && legacy {
        Some(BTreeMap::new())
    } else {
        object.get("pythonPackages").and_then(|value| {
            let entries = value.as_object()?;
            let mut map = BTreeMap::new();
            for (name, version) in entries {
                let version = version.as_str()?;
                if !is_distribution_name(name) || !is_distribution_version(version) {
                    return None;
                }
                map.insert(name.to_owned(), version.to_owned());
            }
            Some(map)
        })
    };
    let (Some(desktop_version), Some(platform), Some(arch), Some(python)) =
        (desktop_version, platform, arch, python)
    else {
        return Err("primary runtime: invalid metadata".to_owned());
    };
    if desktop_version.is_empty()
        || !["win32", "darwin", "linux"].contains(&platform.as_str())
        || !["x64", "arm64"].contains(&arch.as_str())
        || !is_version(&python)
        || node.as_ref().is_some_and(|node| !is_version(node))
        || pnpm
            .as_ref()
            .is_some_and(|pnpm| !is_version(pnpm) || node.is_none())
        || payload_digest.as_ref().is_some_and(|digest| {
            digest.len() != 64
                || !digest
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        })
    {
        return Err("primary runtime: invalid metadata".to_owned());
    }
    let Some(python_packages) = python_packages else {
        return Err("primary runtime: invalid metadata".to_owned());
    };
    if legacy {
        for name in ["numpy", "pandas"] {
            let version = versions.get(name).and_then(Value::as_str);
            let Some(version) = version else {
                return Err("primary runtime: invalid metadata".to_owned());
            };
            if !is_version(version) {
                return Err("primary runtime: invalid metadata".to_owned());
            }
            if let Some(mapped) = python_packages.get(name)
                && mapped != version
            {
                return Err(format!(
                    "primary runtime: conflicting {name} distribution version"
                ));
            }
        }
    }
    Ok(PrimaryRuntime {
        desktop_version,
        platform,
        arch,
        payload_digest,
        python,
        node,
        pnpm,
        python_packages,
    })
}

/// The runtime arch word (dsh `process.arch`; the fr triple maps the
/// two payload arms).
fn runtime_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => "unknown",
    }
}

/// Reads and normalizes the build metadata (dsh `readPrimaryRuntime`).
///
/// # Errors
///
/// The read or decode refusal, then the validation refusal.
pub fn read_primary_runtime(root: &Path) -> Result<PrimaryRuntime, String> {
    let text =
        std::fs::read_to_string(root.join("runtime.json")).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    parse_primary_runtime(&value)
}

/// The POSIX interpreter aliases (dsh `primaryRuntimePythonAliases`):
/// relative alias paths to their sibling interpreter filename.
///
/// # Errors
///
/// The alias-shape refusal.
pub fn python_aliases(
    root: &Path,
    manifest: &PrimaryRuntime,
) -> Result<BTreeMap<String, String>, String> {
    let mut aliases = BTreeMap::new();
    if manifest.platform == "win32" {
        return Ok(aliases);
    }
    let minor = manifest
        .python
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    let interpreter = format!("python{minor}");
    for name in ["python", "python3"] {
        let relative = format!("dependencies/python/bin/{name}");
        let path = root.join(&relative);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let target = std::fs::read_link(&path).map_err(|error| error.to_string())?;
        let sibling = root.join("dependencies/python/bin").join(&interpreter);
        let sibling_is_file = std::fs::symlink_metadata(&sibling)
            .map_err(|error| error.to_string())?
            .is_file();
        if target.to_string_lossy() != interpreter || !sibling_is_file {
            return Err(format!("primary runtime: invalid Python alias {relative}"));
        }
        aliases.insert(relative, interpreter.clone());
    }
    Ok(aliases)
}

/// Copies the payload tree, dereferencing links except the validated
/// interpreter aliases (dsh `copyPrimaryRuntime`).
///
/// # Errors
///
/// The alias validation or the filesystem copy refusal.
pub fn copy_primary_runtime(
    source: &Path,
    destination: &Path,
    manifest: &PrimaryRuntime,
) -> Result<(), String> {
    let aliases = python_aliases(source, manifest)?;
    copy_tree_dereferencing(source, destination, source, &aliases)?;
    for (relative, target) in &aliases {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, destination.join(relative))
            .map_err(|error| error.to_string())?;
        #[cfg(not(unix))]
        {
            let _ = (relative, target);
            return Err("primary runtime: alias links need a POSIX carrier".to_owned());
        }
    }
    Ok(())
}

/// The recursive dereferencing copy (the dsh `cp(dereference: true,
/// filter)` composition): each non-excluded symlink reads through to
/// a copied regular file.
fn copy_tree_dereferencing(
    root: &Path,
    destination: &Path,
    source: &Path,
    excluded: &BTreeMap<String, String>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .expect("walk entries stay under the root")
            .to_string_lossy()
            .into_owned();
        if excluded.contains_key(&relative) {
            continue;
        }
        let target = destination.join(&relative);
        let meta = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if meta.is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
            copy_tree_dereferencing(root, destination, &path, excluded)?;
        } else {
            std::fs::copy(&path, &target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// The resolved interpreter and library locations (dsh
/// `workspaceDependencyPaths`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDependencies {
    /// The absolute interpreter entry point.
    pub python: String,
    /// The absolute Node entry point when bundled.
    pub node: Option<String>,
    /// The absolute pnpm script path when bundled.
    pub pnpm: Option<String>,
    /// The bundled site-packages directory.
    pub python_packages: String,
    /// The bundled `node_modules` directory when bundled.
    pub node_packages: Option<String>,
    /// The locked distribution versions.
    pub python_distributions: BTreeMap<String, String>,
}

/// Resolves the platform-specific locations without touching the
/// environment (dsh `workspaceDependencyPaths`).
#[must_use]
pub fn dependency_paths(root: &Path, manifest: &PrimaryRuntime) -> WorkspaceDependencies {
    let dependencies = root.join("dependencies");
    let windows = manifest.platform == "win32";
    let minor = manifest
        .python
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    let python = if windows {
        dependencies.join("python").join("python.exe")
    } else {
        dependencies.join("python").join("bin").join("python3")
    };
    let python_packages = if windows {
        dependencies
            .join("python")
            .join("Lib")
            .join("site-packages")
    } else {
        dependencies
            .join("python")
            .join("lib")
            .join(format!("python{minor}"))
            .join("site-packages")
    };
    let node = manifest.node.as_ref().map(|_| {
        if windows {
            dependencies.join("node").join("bin").join("node.exe")
        } else {
            dependencies.join("node").join("bin").join("node")
        }
    });
    let node_packages = manifest.node.as_ref().map(|_| {
        dependencies
            .join("node")
            .join("node_modules")
            .to_string_lossy()
            .into_owned()
    });
    let pnpm = manifest
        .pnpm
        .as_ref()
        .map(|_| dependencies.join("pnpm").join("bin").join("pnpm.mjs"));
    WorkspaceDependencies {
        python: python.to_string_lossy().into_owned(),
        node: node.map(|path| path.to_string_lossy().into_owned()),
        pnpm: pnpm.map(|path| path.to_string_lossy().into_owned()),
        python_packages: python_packages.to_string_lossy().into_owned(),
        node_packages,
        python_distributions: manifest.python_packages.clone(),
    }
}

/// Validates the payload entries exist with the expected kinds (dsh
/// `validatePayloadEntries`).
///
/// # Errors
///
/// The dsh expected-kind refusal.
fn validate_payload_entries(paths: &WorkspaceDependencies) -> Result<(), String> {
    let mut checks: Vec<(Option<&String>, bool)> = vec![
        (Some(&paths.python), true),
        (Some(&paths.python_packages), false),
    ];
    if let Some(node) = &paths.node {
        checks.push((Some(node), true));
    }
    if let Some(pnpm) = &paths.pnpm {
        checks.push((Some(pnpm), true));
    }
    if let Some(node_packages) = &paths.node_packages {
        checks.push((Some(node_packages), false));
    }
    for (path, want_file) in checks {
        let Some(path) = path else { continue };
        let meta = std::fs::metadata(path).map_err(|error| error.to_string())?;
        let ok = if want_file {
            meta.is_file()
        } else {
            meta.is_dir()
        };
        if !ok {
            let kind = if want_file { "file" } else { "directory" };
            return Err(format!("primary runtime: expected {kind} at {path}"));
        }
    }
    Ok(())
}

/// The link-free existence probe (dsh `exists`): a symlinked
/// installation path refuses.
///
/// # Errors
///
/// The link refusal or the probe failure.
fn exists_without_link(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "primary runtime: installation path is a filesystem link: {}",
                    path.to_string_lossy()
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

/// The platform/arch compatibility gate (dsh `compatibleManifest`).
///
/// # Errors
///
/// The read refusal or the incompatible-payload refusal.
fn compatible_manifest(source: &Path) -> Result<PrimaryRuntime, String> {
    let manifest = read_primary_runtime(source)?;
    if manifest.platform != std::env::consts::OS || manifest.arch != runtime_arch() {
        return Err("primary runtime: incompatible platform or architecture".to_owned());
    }
    Ok(manifest)
}

/// Uses the payload where it lies: validates metadata, aliases, and
/// entries without copying (dsh `resolvePrimaryRuntime`).
///
/// # Errors
///
/// The compatibility, alias, or entry validation refusal.
pub fn resolve_primary_runtime(source: &Path) -> Result<WorkspaceDependencies, String> {
    let manifest = compatible_manifest(source)?;
    python_aliases(source, &manifest)?;
    let paths = dependency_paths(source, &manifest);
    validate_payload_entries(&paths)?;
    Ok(paths)
}

/// Installs the payload under the fixed root, keeping a complete
/// previous tree on copy failure (dsh `installPrimaryRuntime`).
///
/// # Errors
///
/// Any preparation, copy, or rename refusal.
pub fn install_primary_runtime(
    source: &Path,
    root: &Path,
) -> Result<WorkspaceDependencies, String> {
    let manifest = compatible_manifest(source)?;
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let previous = sibling_path(root, ".previous");
    if !exists_without_link(root)? && exists_without_link(&previous)? {
        std::fs::rename(&previous, root).map_err(|error| error.to_string())?;
    }
    let installed = read_primary_runtime(root).ok();
    if installed.as_ref() == Some(&manifest)
        && exists_without_link(&root.join("runtime.json")).unwrap_or(false)
    {
        python_aliases(root, &manifest)?;
        let paths = dependency_paths(root, &manifest);
        validate_payload_entries(&paths)?;
        return Ok(paths);
    }
    let staging = staging_dir(root)?;
    std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
    let result = (|| -> Result<(), String> {
        copy_primary_runtime(source, &staging, &manifest)?;
        let paths = dependency_paths(&staging, &manifest);
        validate_payload_entries(&paths)?;
        if previous.exists() {
            std::fs::remove_dir_all(&previous).map_err(|error| error.to_string())?;
        }
        let replacing = exists_without_link(root)?;
        if replacing {
            std::fs::rename(root, &previous).map_err(|error| error.to_string())?;
        }
        if let Err(error) = std::fs::rename(&staging, root) {
            if replacing {
                let _ = std::fs::rename(&previous, root);
            }
            return Err(error.to_string());
        }
        if previous.exists() {
            std::fs::remove_dir_all(&previous).map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result?;
    Ok(dependency_paths(root, &manifest))
}

/// The sibling spelling for the rollback tree (dsh `` `${root}.previous`
/// ``).
fn sibling_path(root: &Path, suffix: &str) -> PathBuf {
    let mut name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(suffix);
    root.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

/// The staging directory next to the root (dsh `mkdtemp`).
fn staging_dir(root: &Path) -> Result<PathBuf, String> {
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    for _ in 0..64 {
        let candidate = parent.join(format!(
            ".primary-runtime-{}",
            uuid::Uuid::new_v4().simple()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("primary runtime: staging directory never settled".to_owned())
}

/// Executes one call (dsh `execute`): the memoized preparation over
/// the in-place resolve or the install fork. The fr carrier keeps the
/// memo in the shared runtime state (the dsh `preparation` promise).
///
/// # Errors
///
/// The argument refusal or the preparation refusal.
pub struct WorkspaceDependenciesTool {
    source: PathBuf,
    root: Option<PathBuf>,
    preparation: std::sync::Mutex<Option<Result<WorkspaceDependencies, String>>>,
}

impl std::fmt::Debug for WorkspaceDependenciesTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceDependenciesTool")
            .field("source", &self.source)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl WorkspaceDependenciesTool {
    /// Assembles the tool over the resolved config (dsh `apply`'s
    /// config gate: both paths must be absolute).
    ///
    /// # Errors
    ///
    /// The absolute-path refusal.
    pub fn new(source: &str, root: Option<&str>) -> Result<Self, String> {
        if !Path::new(source).is_absolute()
            || root.is_some_and(|root| !Path::new(root).is_absolute())
        {
            return Err(
                "workspace dependencies: source and root must be absolute paths".to_owned(),
            );
        }
        Ok(Self {
            source: PathBuf::from(source),
            root: root.map(PathBuf::from),
            preparation: std::sync::Mutex::new(None),
        })
    }

    /// Runs the memoized preparation (dsh `execute`'s promise memo:
    /// a failed preparation clears the memo for the next call). The
    /// argument shape gate stands in for the dsh `{}` schema
    /// validation (the cordis-inspect list judgment).
    ///
    /// # Errors
    ///
    /// The non-object argument refusal or the preparation refusal.
    ///
    /// # Panics
    ///
    /// Panics when the preparation-memo lock is poisoned by a
    /// concurrent panic.
    pub fn execute(&self, arguments: &Value) -> Result<Value, BodyError> {
        if !arguments.is_object() {
            return Err(BodyError::plain("arguments must be an object"));
        }
        let outcome = {
            let mut memo = self.preparation.lock().expect("preparation memo");
            if memo.is_none() {
                *memo = Some(self.root.as_ref().map_or_else(
                    || resolve_primary_runtime(&self.source),
                    |root| install_primary_runtime(&self.source, root),
                ));
            }
            let outcome = memo.as_ref().expect("memo populated above").clone();
            // The failed memo clears for the next call (the dsh
            // `preparation = undefined` arm).
            if outcome.is_err() {
                *memo = None;
            }
            outcome
        };
        let value = outcome.map_err(BodyError::plain)?;
        Ok(dependencies_value(&value))
    }

    /// The pending-call card (dsh `presentCall`).
    #[must_use]
    pub fn present_call() -> Value {
        json!({
            "card": "generic",
            "title": "Load workspace dependencies",
            "kind": "read",
        })
    }
}

/// Renders one output value (dsh `output.render`: one text block
/// carrying the pretty JSON).
#[must_use]
pub fn render(_args: &Value, value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    json!([{ "type": "text", "text": text }])
}

/// The wire view of the resolved paths (dsh `WorkspaceDependencies`).
#[must_use]
pub fn dependencies_value(paths: &WorkspaceDependencies) -> Value {
    let mut object = serde_json::Map::new();
    object.insert("python".to_owned(), json!(paths.python));
    if let Some(node) = &paths.node {
        object.insert("node".to_owned(), json!(node));
    }
    if let Some(pnpm) = &paths.pnpm {
        object.insert("pnpm".to_owned(), json!(pnpm));
    }
    object.insert("pythonPackages".to_owned(), json!(paths.python_packages));
    if let Some(node_packages) = &paths.node_packages {
        object.insert("nodePackages".to_owned(), json!(node_packages));
    }
    object.insert(
        "pythonDistributions".to_owned(),
        Value::Object(
            paths
                .python_distributions
                .iter()
                .map(|(name, version)| (name.clone(), json!(version)))
                .collect(),
        ),
    );
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One scratch payload directory with a valid runtime.json and
    /// the dependency skeleton.
    struct Payload(std::path::PathBuf);

    impl Payload {
        fn new(tag: &str, platform: &str, arch: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-wsd-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).expect("payload dir creates");
            #[cfg(unix)]
            {
                std::fs::create_dir_all(dir.join("dependencies/python/bin"))
                    .expect("python bin creates");
                std::fs::write(
                    dir.join("dependencies/python/bin/python3.12"),
                    b"interpreter",
                )
                .expect("interpreter writes");
                std::os::unix::fs::symlink(
                    "python3.12",
                    dir.join("dependencies/python/bin/python3"),
                )
                .expect("alias links");
                std::fs::create_dir_all(
                    dir.join("dependencies/python/lib/python3.12/site-packages"),
                )
                .expect("site-packages creates");
            }
            let runtime = json!({
                "desktopVersion": "1.0.0",
                "platform": platform,
                "arch": arch,
                "python": "3.12.1",
                "pythonPackages": { "numpy": "2.1.0", "pandas": "2.2.1" },
            });
            std::fs::write(
                dir.join("runtime.json"),
                serde_json::to_string(&runtime).expect("manifest serializes"),
            )
            .expect("manifest writes");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Payload {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The manifest validator pins the shape family: a valid
    /// manifest, the legacy components normalization, and the refusal
    /// texts.
    #[test]
    fn manifest_validation_pins_the_family() {
        let runtime = json!({
            "desktopVersion": "1.0.0",
            "platform": "linux",
            "arch": "x64",
            "python": "3.12.1",
            "pythonPackages": { "numpy": "2.1.0" },
        });
        let manifest = parse_primary_runtime(&runtime).expect("manifest");
        assert_eq!(manifest.python, "3.12.1");
        assert_eq!(manifest.python_packages["numpy"], "2.1.0");
        for broken in [
            json!({ "desktopVersion": "", "platform": "linux", "arch": "x64", "python": "3.12.1" }),
            json!({ "desktopVersion": "1.0.0", "platform": "plan9", "arch": "x64", "python": "3.12.1" }),
            json!({ "desktopVersion": "1.0.0", "platform": "linux", "arch": "riscv", "python": "3.12.1" }),
            json!({ "desktopVersion": "1.0.0", "platform": "linux", "arch": "x64", "python": "3" }),
            json!({ "desktopVersion": "1.0.0", "platform": "linux", "arch": "x64", "python": "3.12.1", "pnpm": "9.0.0" }),
        ] {
            assert_eq!(
                parse_primary_runtime(&broken).unwrap_err(),
                "primary runtime: invalid metadata"
            );
        }
    }

    /// The in-place resolve returns the payload paths and validates
    /// the aliases; the install fork copies the tree and re-resolves.
    #[test]
    fn resolve_and_install_round_trip() {
        let platform = std::env::consts::OS.to_owned();
        if platform != "linux" && platform != "macos" {
            return;
        }
        let arch = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "x64"
        };
        let payload = Payload::new("resolve", &platform, arch);
        let resolved = resolve_primary_runtime(payload.path()).expect("resolve");
        assert!(resolved.python.ends_with("bin/python3"));
        assert!(resolved.python_packages.contains("site-packages"));
        assert_eq!(resolved.python_distributions["numpy"], "2.1.0");

        let home = std::env::temp_dir().join(format!(
            "fr-wsd-home-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).expect("home creates");
        let root = home.join("primary-runtime");
        let installed = install_primary_runtime(payload.path(), &root).expect("install");
        assert!(root.join("runtime.json").exists());
        assert!(root.join("dependencies/python/bin/python3.12").exists());
        assert!(installed.python.contains("primary-runtime"));
        // The same-manifest second call short-circuits into the
        // installed tree.
        let again = install_primary_runtime(payload.path(), &root).expect("second install");
        assert_eq!(again, installed);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The tool config gate and the memoized execute face: the
    /// absolute-path refusal and the failure clearing.
    #[test]
    fn tool_gate_and_memo_faces() {
        assert_eq!(
            WorkspaceDependenciesTool::new("relative/path", None).unwrap_err(),
            "workspace dependencies: source and root must be absolute paths"
        );
        let tool = WorkspaceDependenciesTool::new("/nonexistent-payload-root", None)
            .expect("absolute paths accepted");
        assert_eq!(
            tool.execute(&serde_json::json!([1])).unwrap_err().message,
            "arguments must be an object"
        );
        let first = tool.execute(&serde_json::json!({})).unwrap_err();
        assert_eq!(first.message, "No such file or directory (os error 2)");
        // The failed memo cleared: the second call re-prepares (same
        // refusal, still structured).
        let second = tool.execute(&serde_json::json!({})).unwrap_err();
        assert_eq!(second.message, first.message);
    }
}
