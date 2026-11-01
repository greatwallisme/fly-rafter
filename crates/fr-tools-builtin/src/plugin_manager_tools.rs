// Rust guideline compliant 2026-11-02
//! The plugin management tool (dsh `tool-plugin-manager`): the profile
//! plugin/bundle management surface over the fr profile patch files.
//!
//! The fr face is the Q16f-A structural ruling (PRD 19.2 item 26): the
//! npm bundle ecosystem — install/remove through pnpm, build-script
//! approvals, registry fallback chains, `engines.dsh` version
//! exemptions, live HMR — has no carrier, because fr plugins are
//! static Rust artifacts composed at startup. The carried face is the
//! dsh schema/wire/presentation surface 1:1 plus the
//! danger-full-access escalation preamble. The enabled face (the
//! step-5 disk ruling) reads the profile patch rows from disk, writes
//! enablement back into the profile patch with `serde_yaml_ng`
//! (comments are not preserved — PRD 19.2 item 28), and refuses the
//! four ecosystem actions through the dsh `ManagementError` shapes (the
//! fr-authored diagnostic texts — PRD 19.2 item 29).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use serde_yaml_ng::Value as Yaml;

use crate::tools::BodyError;

/// The baked gate: the dsh base bundle mounts the `tool-plugin-manager`
/// row `disabled: true` (the Q16f-A opt-in mirror), so the fr baked
/// composition registers nothing.
pub const BAKED_PLUGIN_MANAGER: bool = false;

/// The fr-authored diagnostic for `install_bundle` (the npm carrier is
/// structurally absent — PRD 19.2 item 29).
const INSTALL_NO_CARRIER: &str = "bundle installation has no carrier in fr: plugins are static \
Rust artifacts assembled at startup, with no npm runtime, no package registry, and no live reload";

/// The fr-authored refusal message for the `list_version_exemptions`
/// read (PRD 19.2 item 29).
const EXEMPTIONS_READ_NO_CARRIER: &str = "version exemptions have no carrier in fr: there is no runtime compatibility machinery to \
query";

/// The fr-authored diagnostic for `set_version_exemption` (PRD 19.2
/// item 29).
const EXEMPTIONS_WRITE_NO_CARRIER: &str = "version exemptions have no carrier in fr: plugins are \
static Rust artifacts with no runtime compatibility machinery";

/// The fr-authored refusal for a call without an active profile (the
/// profile context rides the `FR_PROFILE` launcher fact; the dsh
/// profileContext is always present, so the dsh sources carry no
/// counterpart text).
const PROFILE_REQUIRED: &str = "plugin_manager requires an active profile: set FR_PROFILE";

/// What the model does next for each refusal the manager reports (dsh
/// `ERROR_STEPS`, verbatim; the three brand-worded texts carry the
/// fr mapping — `dsh.bundle` to `fr.bundle`, `DSH` to `fr`).
fn error_step(code: &str) -> Option<&'static str> {
    match code {
        "management-required" => {
            Some("This entry belongs to plugin management itself and cannot be changed.")
        }
        "unaddressable" => Some(
            "This entry has no unique profile patch target; tell the user to edit the profile \
patch file directly.",
        ),
        "unknown-plugin" => Some("Call list_plugins and retry with an exact entryId."),
        "invalid-spec" => Some(
            "Retry with an npm package name, a git URL, a tarball URL, or an absolute local \
directory.",
        ),
        "already-installed" => Some(
            "The package is already installed; to install another version, call remove_bundle \
first, then install_bundle again.",
        ),
        "ambiguous-install" => Some(
            "The dependency change named no single package; call list_bundles to see what was \
installed.",
        ),
        "not-bundle" => Some(
            "The package declares no fr.bundle and cannot be managed as a plugin; install an fr \
plugin bundle instead.",
        ),
        "not-removable" => Some("Call list_bundles; only bundles marked removable can be removed."),
        "stop-profile" => Some(
            "This profile runs without HMR; tell the user to stop fr and run `fr plugin \
--profile <profile> remove <bundle>`.",
        ),
        "bundle-in-use" => Some(
            "Call list_plugins, disable the entries from other layers that still use this \
bundle, then retry.",
        ),
        "stale-approval" => Some(
            "Call install_bundle again without approvedBuilds to read the current \
pendingBuilds.",
        ),
        "incompatible-version" => Some(
            "Install a version compatible with the running fr. A version exemption needs the \
user's explicit acceptance of crash and data-loss risk.",
        ),
        "operation-error" => {
            Some("Report the diagnostic to the user; retry only after its cause is addressed.")
        }
        _ => None,
    }
}

/// What the model does next for each package-manager failure kind (dsh
/// `FAILURE_STEPS`; fr never produces a package run, so the table is
/// reachable only through the wire-shaped `packageResult` the change
/// results structurally carry).
fn failure_step(kind: &str) -> Option<&'static str> {
    match kind {
        "pnpm-missing" => Some("pnpm is not available to fr; tell the user to install pnpm."),
        "timeout" => Some("Retry, or retry with another registry."),
        "not-found" => Some("Check the package name and version with the user, then retry."),
        "no-matching-version" => Some("Retry with a published version."),
        "network" => Some("Retry later or pass another registry."),
        "disk-full" => Some("Tell the user to free disk space, then retry."),
        "permission" => Some(
            "Tell the user fr cannot write the profile directory; retry after they fix its \
permissions.",
        ),
        "build-blocked" => Some(
            "Ask the user whether to allow the install scripts of the packages in \
pendingBuilds; if they approve, retry with approvedBuilds.",
        ),
        _ => None,
    }
}

/// Why a change waits for a restart, as the model tells the user (dsh
/// `RESTART_STEPS`; the three brand-worded texts carry the fr
/// mapping).
fn restart_reason_text(reason: &str) -> Option<&'static str> {
    match reason {
        "no-hmr" => Some("this profile applies changes only at startup"),
        "package-moved" => Some(
            "an installed package now resolves to another directory, or the change replaces a \
package the fr installation provides; remove a package before installing it from another \
location",
        ),
        "host-module" => Some("the change reaches modules fr itself loaded"),
        "unmanaged-plugin" => Some("the change reaches a plugin running outside the plugin list"),
        "module-comparison" => Some("fr could not compare loaded plugins with their files"),
        _ => None,
    }
}

/// Attaches the model-facing `nextStep` to one change result in place
/// (dsh `withNextStep`, arm for arm).
///
/// The plain application carries no key, the restart-required arm
/// composes the reason sentence, and the failed arm prefers the
/// package-manager failure kind over the refusal code.
pub fn with_next_step(result: &mut Value) {
    let application = result
        .get("application")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match application.as_str() {
        "cancelled" => {
            result["nextStep"] =
                json!("The installation was cancelled and its files restored; nothing changed.");
        }
        "overridden" => {
            result["nextStep"] = json!(
                "Saved, but a higher-priority layer such as the agent preset overrides it; tell \
the user it is not in effect."
            );
        }
        "restart-required" => {
            let suffix = result
                .get("restartReason")
                .and_then(Value::as_str)
                .and_then(restart_reason_text)
                .map_or_else(String::new, |text| format!(": {text}"));
            result["nextStep"] = json!(format!(
                "Tell the user to restart fr to apply this change{suffix}."
            ));
        }
        "failed" => {
            let code = result
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("operation-error")
                .to_owned();
            // A refusal code is more specific than the package manager's
            // failure kind: the kind is consulted only under
            // `operation-error` (the dsh comment, carried).
            let kind = if code == "operation-error" {
                result
                    .get("packageResult")
                    .and_then(|run| run.get("kind"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            if let Some(step) = kind
                .as_deref()
                .and_then(failure_step)
                .or_else(|| error_step(&code))
            {
                result["nextStep"] = json!(step);
            }
        }
        _ => {}
    }
}

/// One whole change result with the no-HMR application the fr carrier
/// always produces (the dsh `change()` initial value: without HMR a
/// saved change applies at the next start).
///
/// The restart reason is kept only under `restart-required`.
#[must_use]
pub fn change_result(
    changed: bool,
    stage: &str,
    target: &str,
    enabled: Option<bool>,
    error: Option<Value>,
) -> Value {
    let mut result = json!({
        "changed": changed,
        "application": if error.is_some() { "failed" } else { "restart-required" },
        "stage": stage,
        "target": target,
    });
    if let Some(enabled) = enabled {
        result["enabled"] = json!(enabled);
    }
    match error {
        Some(error) => result["error"] = error,
        None => result["restartReason"] = json!("no-hmr"),
    }
    with_next_step(&mut result);
    result
}

/// Builds one `ManagementError` value (dsh shape: the 13-code union plus
/// the optional diagnostic; fr never emits `incompatible-version`, so
/// the `incompatible` companion stays unconstructed).
#[must_use]
pub fn management_error(code: &str, diagnostic: Option<String>) -> Value {
    let mut error = json!({ "code": code });
    if let Some(diagnostic) = diagnostic {
        error["diagnostic"] = json!(diagnostic);
    }
    error
}

/// Reads the `target`/`enabled` pair the set operations require (the
/// dsh plain-Error refusal verbatim).
///
/// # Errors
///
/// Either field is missing or mistyped.
fn require_target_enabled(arguments: &Value) -> Result<(String, bool), BodyError> {
    let target = arguments.get("target").and_then(Value::as_str);
    let enabled = arguments.get("enabled").and_then(Value::as_bool);
    match (target, enabled) {
        (Some(target), Some(enabled)) => Ok((target.to_owned(), enabled)),
        _ => Err(BodyError::plain("target and enabled are required")),
    }
}

/// Reads the three fields the exemption write requires (the dsh
/// plain-Error refusal verbatim; `acceptRisk` is optional and the fr
/// rejection never reaches the risk gate).
///
/// # Errors
///
/// Any of the three fields is missing or mistyped.
fn require_exemption_fields(arguments: &Value) -> Result<(String, String, bool), BodyError> {
    let target = arguments.get("target").and_then(Value::as_str);
    let runtime_version = arguments.get("runtimeVersion").and_then(Value::as_str);
    let enabled = arguments.get("enabled").and_then(Value::as_bool);
    match (target, runtime_version, enabled) {
        (Some(target), Some(runtime_version), Some(enabled)) => {
            Ok((target.to_owned(), runtime_version.to_owned(), enabled))
        }
        _ => Err(BodyError::plain(
            "target package-name@version, runtimeVersion, and enabled are required",
        )),
    }
}

/// Reads the `target` the install requires (the dsh plain-Error
/// refusal verbatim).
///
/// # Errors
///
/// The field is missing or mistyped.
fn require_install_target(arguments: &Value) -> Result<String, BodyError> {
    arguments
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("target package spec is required"))
}

/// Reads the `target` the removal requires (the dsh plain-Error
/// refusal verbatim).
///
/// # Errors
///
/// The field is missing or mistyped.
fn require_remove_target(arguments: &Value) -> Result<String, BodyError> {
    arguments
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| BodyError::plain("target bundle name is required"))
}

/// Reads one optional integer argument with the dsh `??` default: a
/// present non-null value must be an integral number (the dsh
/// `Number.isInteger` gate; `1.0` passes as JS `1` does).
fn optional_integer(arguments: &Value, key: &str, default: f64) -> Result<f64, BodyError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite() && number.fract() == 0.0)
            .ok_or_else(|| {
                BodyError::plain(
                    "offset must be a non-negative integer and limit must be an integer from 1 \
to 100",
                )
            }),
    }
}

/// Slices one row list into the list-action page (dsh `list_plugins`/
/// `list_bundles` tail).
///
/// The tail is the offset/limit gate, the `total` echo, and the
/// `nextOffset` that is `null` once the page reaches the end. The fr
/// rows are composed in output form, so the dsh `meta` strip is
/// structural — the fr rows never carry display metadata.
///
/// # Errors
///
/// The offset/limit gate refusal (the dsh text verbatim).
pub fn list_page(rows: &[Value], arguments: &Value) -> Result<Value, BodyError> {
    let offset = optional_integer(arguments, "offset", 0.0)?;
    let limit = optional_integer(arguments, "limit", 25.0)?;
    if offset < 0.0 || !(1.0..=100.0).contains(&limit) {
        return Err(BodyError::plain(
            "offset must be a non-negative integer and limit must be an integer from 1 to 100",
        ));
    }
    let total = rows.len();
    #[expect(
        clippy::cast_sign_loss,
        reason = "the gate above rejects negative offsets, so the cast keeps the value"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "integer_arg accepts only integral values, so the cast loses no digit"
    )]
    let start = offset as usize;
    #[expect(
        clippy::cast_sign_loss,
        reason = "the gate above bounds the limit at 1..=100, so the cast keeps the value"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "integer_arg accepts only integral values, so the cast loses no digit"
    )]
    let take = limit as usize;
    let entries: Vec<Value> = rows.iter().skip(start).take(take).cloned().collect();
    let end = start + entries.len();
    let next_offset = if end < total { json!(end) } else { Value::Null };
    Ok(json!({ "entries": entries, "total": total, "nextOffset": next_offset }))
}

/// The disk-face profile paths (the step-5 disk ruling): the profile
/// directory, its patch file, and its manifest file, under the
/// resolved home.
#[derive(Debug)]
pub struct ProfilePaths {
    /// The profile directory (`<FR_HOME>/profiles/<FR_PROFILE>`).
    pub dir: PathBuf,
    /// The profile patch file the set operations write.
    pub patch: PathBuf,
    /// The home-root patch file the list view composes last.
    pub home_patch: PathBuf,
    /// The profile manifest the bundle selection rides.
    pub manifest: PathBuf,
}

/// Resolves the profile paths from the launcher facts: the home from
/// the dsh `resolveDshHome` chain, the profile from `FR_PROFILE`
/// (the tool-side read; the shell env face keeps its pinned absence).
///
/// # Errors
///
/// The home resolution failure.
pub fn resolve_profile_paths(home_override: Option<&str>) -> Result<Option<ProfilePaths>, String> {
    let home = crate::shell_env::resolve_home(home_override)?;
    let profile = match std::env::var("FR_PROFILE") {
        Ok(profile) if !profile.is_empty() => profile,
        _ => return Ok(None),
    };
    let dir = Path::new(&home).join("profiles").join(&profile);
    Ok(Some(ProfilePaths {
        home_patch: Path::new(&home).join("fr.patch.yml"),
        patch: dir.join("fr.patch.yml"),
        manifest: dir.join("profile.yml"),
        dir,
    }))
}

/// Reads one patch layer as a row sequence (the dsh
/// `loadOptionalPatches` shape: a missing file composes nothing, any
/// other read or parse failure refuses).
///
/// # Errors
///
/// The read or parse refusal (the io or YAML diagnostic verbatim).
fn read_patch_rows(path: &Path) -> Result<Vec<Yaml>, BodyError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(BodyError::plain(error.to_string())),
    };
    let document: Yaml =
        serde_yaml_ng::from_str(&text).map_err(|error| BodyError::plain(error.to_string()))?;
    match document {
        Yaml::Sequence(rows) => Ok(rows),
        _ => Err(BodyError::plain("Profile patch must be a YAML sequence")),
    }
}

/// Flattens the group containers depth-first (dsh `flatten`: each row
/// precedes its `group: true` children).
fn flatten_rows(rows: Vec<Yaml>) -> Vec<Yaml> {
    let mut out = Vec::new();
    for row in rows {
        let children = row
            .get("group")
            .and_then(Yaml::as_bool)
            .filter(|group| *group)
            .and_then(|_| row.get("config"))
            .and_then(Yaml::as_sequence)
            .map(|children| flatten_rows(children.clone()));
        out.push(row);
        if let Some(children) = children {
            out.extend(children);
        }
    }
    out
}

/// The composed patch view: the profile patch rows first, the
/// home-root rows last (the dsh `readProfilePatches` order minus the
/// invisible layers — the bundle patches and the launch overlays).
fn composed_rows(profile: &ProfilePaths) -> Result<Vec<Yaml>, BodyError> {
    let mut rows = Vec::new();
    for layer in [
        read_patch_rows(&profile.patch)?,
        read_patch_rows(&profile.home_patch)?,
    ] {
        for row in layer {
            match row.get("insert").and_then(Yaml::as_sequence).cloned() {
                Some(children) => rows.extend(children),
                None => rows.push(row),
            }
        }
    }
    Ok(flatten_rows(rows))
}

/// Reads one string field off a patch row.
fn row_string<'a>(row: &'a Yaml, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Yaml::as_str)
}

/// The patch view's winner rows: first-insertion order, the last row
/// per id winning the values (the fr-config composition semantics).
fn winner_rows(rows: Vec<Yaml>) -> Vec<(String, Yaml)> {
    let mut order: Vec<String> = Vec::new();
    let mut winners: std::collections::HashMap<String, Yaml> = std::collections::HashMap::new();
    for row in rows {
        let Some(id) = row_string(&row, "id").map(str::to_owned) else {
            continue;
        };
        if !order.iter().any(|seen| seen == &id) {
            order.push(id.clone());
        }
        winners.insert(id, row);
    }
    order
        .into_iter()
        .filter_map(|id| winners.remove(&id).map(|row| (id, row)))
        .collect()
}

/// One plugin row shaped for the list face (the dsh `PluginInfo`
/// fields minus the invisible ones: the display metadata and the live
/// fiber phase, which the disk face cannot see — PRD 19.2 item 27).
/// The literal-only enablement mirrors the composition winner: a
/// `!!js` disabled expression is not a literal `true`, so the row
/// reads enabled.
fn plugin_row(id: &str, row: &Yaml) -> Option<Value> {
    let name = row_string(row, "name")?;
    let disabled = row.get("disabled").and_then(Yaml::as_bool) == Some(true);
    Some(json!({
        "entryId": id,
        "moduleName": name,
        "enabled": !disabled,
        "fiberPhase": Value::Null,
        "patchId": id,
    }))
}

/// The list rows for the patch view: the winner rows that carry a
/// module name, in composition order.
fn plugin_rows(profile: &ProfilePaths) -> Result<Vec<Value>, BodyError> {
    let rows = winner_rows(composed_rows(profile)?);
    Ok(rows
        .into_iter()
        .filter_map(|(id, row)| plugin_row(&id, &row))
        .collect())
}

/// The row's literal `disabled` value (`!!js` rows never match, so
/// their override is kept — the dsh comment, carried).
fn literal_disabled(row: &Yaml) -> Option<bool> {
    row.get("disabled").and_then(Yaml::as_bool)
}

/// Whether the profile patch rows without the skipped one (or all of
/// them) already leave the id disabled exactly as requested. The
/// bundle patches are invisible to the disk face, so the composition
/// is the profile rows alone (the dsh `implied` minus the bundle
/// layers).
fn implied_disabled(rows: &[Yaml], skip: Option<usize>, id: &str, enabled: bool) -> bool {
    rows.iter()
        .enumerate()
        .rev()
        .filter(|(index, _)| Some(*index) != skip)
        .find(|(_, row)| row_string(row, "id") == Some(id))
        .and_then(|(_, row)| literal_disabled(row))
        .is_some_and(|disabled| disabled != enabled)
}

/// Whether the row holds only scalar `id`/`disabled` switches (the
/// dsh `onlySwitch` minus the anchor half — `serde_yaml_ng` keeps no
/// anchors, so a tagged value is the only non-scalar a switch row can
/// carry).
fn only_switch(row: &Yaml) -> bool {
    let Some(mapping) = row.as_mapping() else {
        return false;
    };
    mapping.iter().all(|(key, value)| {
        key.as_str()
            .is_some_and(|key| key == "id" || key == "disabled")
            && !matches!(
                value,
                Yaml::Sequence(_) | Yaml::Mapping(_) | Yaml::Tagged(_)
            )
    })
}

/// Replaces the last matching override or appends one after the
/// existing rows (dsh `writePluginEnabled`, minus the comment- and
/// anchor-preserving half — the `serde_yaml_ng` rewrite drops
/// comments, the PRD 19.2 item 28 record). Returns whether the file
/// changed.
///
/// # Errors
///
/// The read, parse, or write refusal.
fn write_plugin_enabled(
    patch: &Path,
    id: &str,
    name: &str,
    enabled: bool,
) -> Result<bool, BodyError> {
    let text = match std::fs::read_to_string(patch) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "[]\n".to_owned(),
        Err(error) => return Err(BodyError::plain(error.to_string())),
    };
    let mut document: Yaml =
        serde_yaml_ng::from_str(&text).map_err(|error| BodyError::plain(error.to_string()))?;
    let Some(rows) = document.as_sequence_mut() else {
        return Err(BodyError::plain("Profile patch must be a YAML sequence"));
    };
    // findLast over the rows the write matches: the id equality, no
    // `insert` group, and the name either absent or equal.
    let mut target: Option<usize> = None;
    for (index, row) in rows.iter().enumerate() {
        let matched = row_string(row, "id") == Some(id)
            && row.get("insert").is_none()
            && row_string(row, "name").is_none_or(|found| found == name);
        if matched {
            target = Some(index);
        }
    }
    let changed = match target {
        Some(index) => {
            let row = rows[index].clone();
            if only_switch(&row) && implied_disabled(rows, Some(index), id, enabled) {
                rows.remove(index);
                true
            } else if literal_disabled(&row) == Some(!enabled) {
                false
            } else {
                if let Some(mapping) = rows[index].as_mapping_mut() {
                    mapping.insert(Yaml::String("disabled".to_owned()), Yaml::Bool(!enabled));
                }
                true
            }
        }
        None => {
            if implied_disabled(rows, None, id, enabled) {
                false
            } else {
                let mut row = serde_yaml_ng::Mapping::new();
                row.insert(Yaml::String("id".to_owned()), Yaml::String(id.to_owned()));
                row.insert(Yaml::String("disabled".to_owned()), Yaml::Bool(!enabled));
                rows.push(Yaml::Mapping(row));
                true
            }
        }
    };
    if !changed {
        return Ok(false);
    }
    let rendered =
        serde_yaml_ng::to_string(&document).map_err(|error| BodyError::plain(error.to_string()))?;
    std::fs::write(patch, rendered).map_err(|error| BodyError::plain(error.to_string()))?;
    Ok(true)
}

/// Executes the `set_plugin` action (dsh `setPluginEnabled` over the
/// disk face): the list-row lookup, the unknown-plugin refusal, and
/// the profile patch write.
///
/// # Errors
///
/// The patch view or write refusal.
fn set_plugin(profile: &ProfilePaths, target: &str, enabled: bool) -> Result<Value, BodyError> {
    let rows = plugin_rows(profile)?;
    let Some(row) = rows.iter().find(|row| row["entryId"] == json!(target)) else {
        return Ok(change_result(
            false,
            "enable",
            target,
            Some(enabled),
            Some(management_error("unknown-plugin", None)),
        ));
    };
    let name = row["moduleName"].as_str().unwrap_or_default().to_owned();
    let changed = write_plugin_enabled(&profile.patch, target, &name, enabled)?;
    Ok(change_result(
        changed,
        "enable",
        target,
        Some(enabled),
        None,
    ))
}

/// Reads the profile manifest document and its bundle selection (the
/// fr `profile.yml` `bundles:` list). A read or parse failure lands in
/// the operation-error shape the dsh `change()` produces for a thrown
/// read, not in a tool error.
fn manifest_document(profile: &ProfilePaths) -> Result<(Yaml, Vec<String>), Value> {
    let text = std::fs::read_to_string(&profile.manifest)
        .map_err(|error| management_error("operation-error", Some(error.to_string())))?;
    let manifest: Yaml = serde_yaml_ng::from_str(&text)
        .map_err(|error| management_error("operation-error", Some(error.to_string())))?;
    let selection = manifest
        .get("bundles")
        .and_then(Yaml::as_sequence)
        .into_iter()
        .flatten()
        .filter_map(Yaml::as_str)
        .map(str::to_owned)
        .collect();
    Ok((manifest, selection))
}

/// Executes the `set_bundle` action (dsh `setBundleEnabled` over the
/// disk face): the manifest read, the metadata gate that the disk
/// face cannot satisfy for any enable (or unselected disable), and
/// the selection edit.
///
/// # Errors
///
/// The manifest write refusal.
fn set_bundle(profile: &ProfilePaths, name: &str, enabled: bool) -> Result<Value, BodyError> {
    let (mut manifest, previous) = match manifest_document(profile) {
        Ok(document) => document,
        Err(error) => {
            return Ok(change_result(
                false,
                "enable",
                name,
                Some(enabled),
                Some(error),
            ));
        }
    };
    // The bundle-metadata gate: fr carries no bundle manifests, so
    // every enable — and every disable of an unselected name — refuses
    // through the dsh not-bundle arm (the step-5 ruling).
    if enabled || !previous.iter().any(|seen| seen == name) {
        return Ok(change_result(
            false,
            "enable",
            name,
            Some(enabled),
            Some(management_error("not-bundle", None)),
        ));
    }
    let Some(selection) = manifest.get_mut("bundles").and_then(Yaml::as_sequence_mut) else {
        return Ok(change_result(
            false,
            "enable",
            name,
            Some(enabled),
            Some(management_error(
                "operation-error",
                Some(String::from(
                    "the profile manifest carries no bundle selection",
                )),
            )),
        ));
    };
    selection.retain(|entry| entry.as_str() != Some(name));
    let rendered =
        serde_yaml_ng::to_string(&manifest).map_err(|error| BodyError::plain(error.to_string()))?;
    std::fs::write(&profile.manifest, rendered)
        .map_err(|error| BodyError::plain(error.to_string()))?;
    Ok(change_result(true, "enable", name, Some(enabled), None))
}

/// Executes the `remove_bundle` action (dsh `removeBundle` over the
/// disk face): the bundle rows are the invisible carrier, so the
/// lookup refuses through the dsh not-removable arm.
#[must_use]
fn remove_bundle(name: &str) -> Value {
    change_result(
        false,
        "remove",
        name,
        None,
        Some(management_error("not-removable", None)),
    )
}

/// Executes the `install_bundle` action over the carrierless fr face
/// (the Q16f-A ruling): the dsh change shape with the fr-authored
/// operation-error diagnostic.
///
/// # Errors
///
/// The target gate.
fn install_bundle(arguments: &Value) -> Result<Value, BodyError> {
    let target = require_install_target(arguments)?;
    // dsh `options?.enabled !== false`: the default is activation.
    let enabled = arguments.get("enabled").and_then(Value::as_bool) != Some(false);
    Ok(change_result(
        false,
        "install",
        &target,
        Some(enabled),
        Some(management_error(
            "operation-error",
            Some(String::from(INSTALL_NO_CARRIER)),
        )),
    ))
}

/// Executes the `set_version_exemption` action over the carrierless
/// fr face (the Q16f-A ruling): the field gate first, then the dsh
/// change shape with the fr-authored diagnostic.
///
/// # Errors
///
/// The field gate.
fn set_version_exemption(arguments: &Value) -> Result<Value, BodyError> {
    let (target, _runtime_version, enabled) = require_exemption_fields(arguments)?;
    Ok(change_result(
        false,
        "enable",
        &target,
        Some(enabled),
        Some(management_error(
            "operation-error",
            Some(String::from(EXEMPTIONS_WRITE_NO_CARRIER)),
        )),
    ))
}

/// Reads the version exemptions (the dsh read action): fr carries no
/// compatibility machinery, so the read refuses (the step-5 shape
/// ruling — a tool error, not a change result).
///
/// # Errors
///
/// Always: the carrier refusal.
fn list_version_exemptions() -> Result<Value, BodyError> {
    Err(BodyError::plain(EXEMPTIONS_READ_NO_CARRIER))
}

/// The eight management actions (the dsh enum order).
const ACTIONS: [&str; 8] = [
    "list_plugins",
    "list_bundles",
    "set_plugin",
    "set_bundle",
    "install_bundle",
    "remove_bundle",
    "list_version_exemptions",
    "set_version_exemption",
];

/// Wraps one reply object as the JSON string the tool's string output
/// schema carries (the dsh `JSON.stringify` tail).
///
/// # Panics
///
/// Never for a `Value` input: the serialization is infallible, so the
/// `expect` documents the invariant without a reachable failure arm.
fn json_string(value: &Value) -> Value {
    Value::String(serde_json::to_string(value).expect("Value serialization cannot fail"))
}

/// Executes the management tool (dsh `execute`, arm for arm): the
/// standing policy resolution, the danger-full-access escalation
/// preamble, the profile resolution, and the action dispatch.
///
/// # Errors
///
/// The policy, approval, profile, argument, or arm refusal.
///
/// # Panics
///
/// Never for a `Value` argument: the justification serialization is
/// infallible, so the `expect` documents the invariant.
pub fn execute(
    policy: &dyn crate::tools::PolicySeam,
    approval: &dyn crate::tools::ApprovalSeam,
    arguments: &Value,
    agent: Option<&str>,
    call_id: &str,
) -> Result<Value, BodyError> {
    let standing = policy.resolve_policy(agent).map_err(BodyError::plain)?;
    let effective = fr_sandbox::seam::SandboxMode::parse(
        standing
            .as_ref()
            .and_then(|value| value.get("mode"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .ok_or_else(|| BodyError::plain("the standing policy mode is not a sandbox mode"))?;
    let args_json = serde_json::to_string(arguments).expect("Value serialization cannot fail");
    let request = fr_sandbox::escalation::EscalationRequest {
        requested_mode: String::from("danger-full-access"),
        justification: format!(
            "plugin_manager {args_json}. Profile changes persist across sessions; installed \
Host code runs outside the workspace sandbox."
        ),
        effective_mode: effective,
        subject: "plugin management operation",
    };
    let approval_request = fr_sandbox::escalation::EscalationApproval {
        approver: Some(&crate::tools::SeamApprover(approval)),
        agent,
        call_id,
        tool_name: "plugin_manager",
    };
    // The one-call grant is consumed by this call alone (the dsh
    // escalation preamble runs before anything else).
    fr_sandbox::escalation::approve_escalation(&request, &approval_request)
        .map_err(BodyError::plain)?;
    let Some(profile) = resolve_profile_paths(None).map_err(BodyError::plain)? else {
        return Err(BodyError::plain(PROFILE_REQUIRED));
    };
    let refuse_action = || {
        BodyError::plain(format!(
            "action must be a string in {{{}}}",
            ACTIONS.join(", ")
        ))
    };
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(refuse_action)?;
    if !ACTIONS.contains(&action) {
        return Err(refuse_action());
    }
    match action {
        "list_plugins" | "list_bundles" => {
            let rows = if action == "list_plugins" {
                plugin_rows(&profile)?
            } else {
                Vec::new()
            };
            list_page(&rows, arguments).map(|page| json_string(&page))
        }
        "set_plugin" => {
            let (target, enabled) = require_target_enabled(arguments)?;
            set_plugin(&profile, &target, enabled).map(|value| json_string(&value))
        }
        "set_bundle" => {
            let (target, enabled) = require_target_enabled(arguments)?;
            set_bundle(&profile, &target, enabled).map(|value| json_string(&value))
        }
        "install_bundle" => install_bundle(arguments).map(|value| json_string(&value)),
        "remove_bundle" => {
            let target = require_remove_target(arguments)?;
            Ok(remove_bundle(&target))
        }
        "list_version_exemptions" => list_version_exemptions(),
        "set_version_exemption" => {
            set_version_exemption(arguments).map(|value| json_string(&value))
        }
        _ => Err(refuse_action()),
    }
}

/// The pending-call presentation (dsh `presentCall`: the generic card
/// with the list/read fork).
#[must_use]
pub fn present_call(arguments: &Value) -> Value {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = if action.starts_with("list_") {
        "read"
    } else {
        "other"
    };
    json!({
        "card": "generic",
        "title": "Manage profile plugins",
        "kind": kind,
        "rawInput": arguments,
    })
}

/// Renders one output value (the dsh `output.render` pair: one text
/// block carrying the JSON string verbatim).
#[must_use]
pub fn render(_arguments: &Value, value: &Value) -> Value {
    let text = value.as_str().unwrap_or_default().to_owned();
    json!([{ "type": "text", "text": text }])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The thirteen refusal texts carry the dsh wording verbatim, with
    /// the three brand-worded entries on the fr mapping.
    #[test]
    fn error_steps_carry_the_thirteen_dsh_texts() {
        let expected = [
            (
                "management-required",
                "This entry belongs to plugin management itself and cannot be changed.",
            ),
            (
                "unaddressable",
                "This entry has no unique profile patch target; tell the user to edit the profile patch file directly.",
            ),
            (
                "unknown-plugin",
                "Call list_plugins and retry with an exact entryId.",
            ),
            (
                "invalid-spec",
                "Retry with an npm package name, a git URL, a tarball URL, or an absolute local directory.",
            ),
            (
                "already-installed",
                "The package is already installed; to install another version, call remove_bundle first, then install_bundle again.",
            ),
            (
                "ambiguous-install",
                "The dependency change named no single package; call list_bundles to see what was installed.",
            ),
            (
                "not-bundle",
                "The package declares no fr.bundle and cannot be managed as a plugin; install an fr plugin bundle instead.",
            ),
            (
                "not-removable",
                "Call list_bundles; only bundles marked removable can be removed.",
            ),
            (
                "stop-profile",
                "This profile runs without HMR; tell the user to stop fr and run `fr plugin --profile <profile> remove <bundle>`.",
            ),
            (
                "bundle-in-use",
                "Call list_plugins, disable the entries from other layers that still use this bundle, then retry.",
            ),
            (
                "stale-approval",
                "Call install_bundle again without approvedBuilds to read the current pendingBuilds.",
            ),
            (
                "incompatible-version",
                "Install a version compatible with the running fr. A version exemption needs the user's explicit acceptance of crash and data-loss risk.",
            ),
            (
                "operation-error",
                "Report the diagnostic to the user; retry only after its cause is addressed.",
            ),
        ];
        for (code, text) in expected {
            assert_eq!(error_step(code), Some(text), "code {code}");
        }
        assert_eq!(error_step("edge"), None);
    }

    /// The eight failure-kind texts carry the dsh wording (the fr
    /// brand on the two worded entries), and the two kinds dsh leaves
    /// out of the table stay absent.
    #[test]
    fn failure_steps_carry_the_dsh_texts() {
        assert_eq!(
            failure_step("pnpm-missing"),
            Some("pnpm is not available to fr; tell the user to install pnpm.")
        );
        assert_eq!(
            failure_step("timeout"),
            Some("Retry, or retry with another registry.")
        );
        assert_eq!(
            failure_step("permission"),
            Some(
                "Tell the user fr cannot write the profile directory; retry after they fix its \
permissions."
            )
        );
        assert_eq!(failure_step("integrity"), None);
        assert_eq!(failure_step("unknown"), None);
    }

    /// The five restart reasons carry the dsh wording with the fr
    /// brand mapping on the three worded entries.
    #[test]
    fn restart_reasons_carry_the_dsh_texts() {
        let expected = [
            ("no-hmr", "this profile applies changes only at startup"),
            (
                "package-moved",
                "an installed package now resolves to another directory, or the change replaces \
a package the fr installation provides; remove a package before installing it from another \
location",
            ),
            ("host-module", "the change reaches modules fr itself loaded"),
            (
                "unmanaged-plugin",
                "the change reaches a plugin running outside the plugin list",
            ),
            (
                "module-comparison",
                "fr could not compare loaded plugins with their files",
            ),
        ];
        for (reason, text) in expected {
            assert_eq!(restart_reason_text(reason), Some(text), "reason {reason}");
        }
        assert_eq!(restart_reason_text("edge"), None);
    }

    /// The five application arms attach exactly the dsh nextStep
    /// values: the plain application none, the restart-required arm
    /// the composed reason sentence, and the failed arm the
    /// kind-over-code preference.
    #[test]
    fn with_next_step_covers_the_five_application_arms() {
        let mut applied = json!({ "application": "applied" });
        with_next_step(&mut applied);
        assert!(applied.get("nextStep").is_none());

        let mut cancelled = json!({ "application": "cancelled" });
        with_next_step(&mut cancelled);
        assert_eq!(
            cancelled["nextStep"],
            "The installation was cancelled and its files restored; nothing changed."
        );

        let mut overridden = json!({ "application": "overridden" });
        with_next_step(&mut overridden);
        assert_eq!(
            overridden["nextStep"],
            "Saved, but a higher-priority layer such as the agent preset overrides it; tell the \
user it is not in effect."
        );

        let mut plain = json!({ "application": "restart-required" });
        with_next_step(&mut plain);
        assert_eq!(
            plain["nextStep"],
            "Tell the user to restart fr to apply this change."
        );

        let mut reasoned = json!({ "application": "restart-required", "restartReason": "no-hmr" });
        with_next_step(&mut reasoned);
        assert_eq!(
            reasoned["nextStep"],
            "Tell the user to restart fr to apply this change: this profile applies changes \
only at startup."
        );

        let mut coded = json!({
            "application": "failed",
            "error": { "code": "unknown-plugin" },
        });
        with_next_step(&mut coded);
        assert_eq!(
            coded["nextStep"],
            "Call list_plugins and retry with an exact entryId."
        );

        // The failure kind wins over the refusal code only under
        // `operation-error`.
        let mut kinded = json!({
            "application": "failed",
            "error": { "code": "operation-error" },
            "packageResult": { "kind": "timeout" },
        });
        with_next_step(&mut kinded);
        assert_eq!(kinded["nextStep"], "Retry, or retry with another registry.");

        let mut unkinded = json!({
            "application": "failed",
            "error": { "code": "operation-error" },
            "packageResult": { "kind": "integrity" },
        });
        with_next_step(&mut unkinded);
        assert_eq!(
            unkinded["nextStep"],
            "Report the diagnostic to the user; retry only after its cause is addressed."
        );

        // A non-operation-error code ignores the package result.
        let mut mispriority = json!({
            "application": "failed",
            "error": { "code": "not-removable" },
            "packageResult": { "kind": "timeout" },
        });
        with_next_step(&mut mispriority);
        assert_eq!(
            mispriority["nextStep"],
            "Call list_bundles; only bundles marked removable can be removed."
        );
    }

    /// The change-result builder pins the no-HMR shape: a refusal
    /// drops the restart reason, a success carries it, and the
    /// enabled field rides only when the caller supplies it.
    #[test]
    fn change_result_builds_the_no_hmr_shape() {
        let failed = change_result(
            false,
            "install",
            "spec",
            Some(true),
            Some(management_error("operation-error", Some("d".to_owned()))),
        );
        assert_eq!(failed["changed"], json!(false));
        assert_eq!(failed["application"], "failed");
        assert_eq!(failed["stage"], "install");
        assert_eq!(failed["enabled"], json!(true));
        assert_eq!(failed["error"]["code"], "operation-error");
        assert_eq!(failed["error"]["diagnostic"], "d");
        assert!(failed.get("restartReason").is_none());
        assert_eq!(
            failed["nextStep"],
            "Report the diagnostic to the user; retry only after its cause is addressed."
        );

        let saved = change_result(true, "enable", "web", Some(false), None);
        assert_eq!(saved["application"], "restart-required");
        assert_eq!(saved["restartReason"], "no-hmr");
        assert_eq!(saved["enabled"], json!(false));
        assert_eq!(saved["target"], "web");

        let mut bare = change_result(true, "enable", "web", None, None);
        assert!(bare.get("enabled").is_none());
        bare.as_object_mut().expect("object").remove("nextStep");
        assert_eq!(
            bare,
            json!({
                "changed": true,
                "application": "restart-required",
                "restartReason": "no-hmr",
                "stage": "enable",
                "target": "web",
            })
        );
    }

    /// The argument gates refuse with the dsh plain-Error texts,
    /// pass-through otherwise.
    #[test]
    fn argument_gates_carry_the_dsh_texts() {
        let pair = json!({ "target": "web", "enabled": false });
        assert_eq!(
            require_target_enabled(&pair).expect("pair"),
            ("web".to_owned(), false)
        );
        assert_eq!(
            require_target_enabled(&json!({ "target": "web" }))
                .unwrap_err()
                .message,
            "target and enabled are required"
        );
        assert_eq!(
            require_target_enabled(&json!({ "enabled": true }))
                .unwrap_err()
                .message,
            "target and enabled are required"
        );

        let triple = json!({ "target": "a@1", "runtimeVersion": "0.1.0", "enabled": true });
        assert_eq!(
            require_exemption_fields(&triple).expect("triple"),
            ("a@1".to_owned(), "0.1.0".to_owned(), true)
        );
        assert_eq!(
            require_exemption_fields(&json!({ "target": "a@1", "enabled": true }))
                .unwrap_err()
                .message,
            "target package-name@version, runtimeVersion, and enabled are required"
        );

        assert_eq!(
            require_install_target(&json!({ "target": "pkg" })).expect("spec"),
            "pkg"
        );
        assert_eq!(
            require_install_target(&json!({})).unwrap_err().message,
            "target package spec is required"
        );
        assert_eq!(
            require_remove_target(&json!({ "target": "pkg" })).expect("name"),
            "pkg"
        );
        assert_eq!(
            require_remove_target(&json!({})).unwrap_err().message,
            "target bundle name is required"
        );
    }

    /// The list page pins the dsh tail: defaults, the gate text, the
    /// total echo, and the nextOffset null once the page reaches the
    /// end (or starts past it).
    #[test]
    fn list_page_pins_the_dsh_tail() {
        let rows: Vec<Value> = (0..7)
            .map(|index| json!({ "entryId": format!("row-{index}") }))
            .collect();
        let page = list_page(&rows, &json!({})).expect("default page");
        assert_eq!(page["total"], json!(7));
        assert_eq!(page["entries"].as_array().expect("array").len(), 7);
        assert_eq!(page["nextOffset"], Value::Null);

        let page = list_page(&rows, &json!({ "offset": 2, "limit": 3 })).expect("middle page");
        let names: Vec<String> = page["entries"]
            .as_array()
            .expect("array")
            .iter()
            .map(|row| row["entryId"].as_str().expect("string").to_owned())
            .collect();
        assert_eq!(names, ["row-2", "row-3", "row-4"]);
        assert_eq!(page["nextOffset"], json!(5));

        let tail = list_page(&rows, &json!({ "offset": 5, "limit": 25 })).expect("tail page");
        assert_eq!(tail["entries"].as_array().expect("array").len(), 2);
        assert_eq!(tail["nextOffset"], Value::Null);

        let past = list_page(&rows, &json!({ "offset": 9 })).expect("past page");
        assert_eq!(past["entries"].as_array().expect("array").len(), 0);
        assert_eq!(past["nextOffset"], Value::Null);

        assert_eq!(
            list_page(&rows, &json!({ "offset": -1 }))
                .unwrap_err()
                .message,
            "offset must be a non-negative integer and limit must be an integer from 1 to 100"
        );
        assert_eq!(
            list_page(&rows, &json!({ "limit": 0 }))
                .unwrap_err()
                .message,
            "offset must be a non-negative integer and limit must be an integer from 1 to 100"
        );
        assert_eq!(
            list_page(&rows, &json!({ "limit": 101 }))
                .unwrap_err()
                .message,
            "offset must be a non-negative integer and limit must be an integer from 1 to 100"
        );
        assert_eq!(
            list_page(&rows, &json!({ "limit": 2.5 }))
                .unwrap_err()
                .message,
            "offset must be a non-negative integer and limit must be an integer from 1 to 100"
        );
        // JS Number.isInteger accepts 1.0.
        let whole = list_page(&rows, &json!({ "limit": 3.0 })).expect("whole page");
        assert_eq!(whole["entries"].as_array().expect("array").len(), 3);
    }

    /// One scratch profile tree with a Drop cleanup (the wsd
    /// Payload judgment).
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "fr-pm-{tag}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir creates");
            Self(dir)
        }

        fn paths(&self) -> ProfilePaths {
            ProfilePaths {
                dir: self.0.clone(),
                patch: self.0.join("fr.patch.yml"),
                home_patch: self.0.join("home.patch.yml"),
                manifest: self.0.join("profile.yml"),
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The patch view composes the profile rows before the home rows,
    /// flattens the group containers, and shapes the winner rows: the
    /// last row per id wins, name-less overrides stay out of the list,
    /// and only a literal `disabled: true` reads disabled.
    #[test]
    fn patch_view_composes_and_shapes_the_rows() {
        let scratch = Scratch::new("view");
        let paths = scratch.paths();
        std::fs::write(
            &paths.patch,
            "- id: web\n  name: tool-web\n  disabled: true\n- id: legacy\n  name: old-tool\n  \
disabled: false\n- insert:\n  - id: inner\n    name: group-child\n- id: web\n  disabled: \
false\n",
        )
        .expect("profile patch writes");
        std::fs::write(
            &paths.home_patch,
            "- id: web\n  name: tool-web\n- id: legacy\n  name: old-tool\n  disabled: \
true\n",
        )
        .expect("home patch writes");
        let rows = plugin_rows(&paths).expect("rows");
        assert_eq!(
            rows,
            vec![
                json!({
                    "entryId": "web",
                    "moduleName": "tool-web",
                    "enabled": true,
                    "fiberPhase": Value::Null,
                    "patchId": "web",
                }),
                json!({
                    "entryId": "legacy",
                    "moduleName": "old-tool",
                    "enabled": false,
                    "fiberPhase": Value::Null,
                    "patchId": "legacy",
                }),
                json!({
                    "entryId": "inner",
                    "moduleName": "group-child",
                    "enabled": true,
                    "fiberPhase": Value::Null,
                    "patchId": "inner",
                }),
            ]
        );
    }

    /// The set-plugin arm flips the winner's literal, appends for an
    /// unknown patch target, refuses an unknown list row, and leaves
    /// the file byte-identical on the no-op path.
    #[test]
    fn set_plugin_writes_and_refuses() {
        let scratch = Scratch::new("set");
        let paths = scratch.paths();
        std::fs::write(
            &paths.patch,
            "- id: web\n  name: tool-web\n  disabled: true\n",
        )
        .expect("profile patch writes");
        let before = std::fs::read_to_string(&paths.patch).expect("read");

        // The no-op disable: the literal already equals the request.
        let reply = set_plugin(&paths, "web", false).expect("no-op set");
        assert_eq!(reply["changed"], json!(false));
        assert_eq!(reply["application"], "restart-required");
        assert_eq!(reply["restartReason"], "no-hmr");
        assert_eq!(
            reply["nextStep"],
            "Tell the user to restart fr to apply this change: this profile applies changes \
only at startup."
        );
        assert_eq!(std::fs::read_to_string(&paths.patch).expect("read"), before);

        // The real flip.
        let reply = set_plugin(&paths, "web", true).expect("flip");
        assert_eq!(reply["changed"], json!(true));
        let text = std::fs::read_to_string(&paths.patch).expect("read");
        assert!(
            text.contains("disabled: false"),
            "flip keeps the value: {text}"
        );
        assert!(
            text.contains("name: tool-web"),
            "flip keeps the name: {text}"
        );

        // An id outside the list view refuses without touching the file.
        let reply = set_plugin(&paths, "ghost", true).expect("unknown set");
        assert_eq!(reply["application"], "failed");
        assert_eq!(reply["error"]["code"], "unknown-plugin");
        assert_eq!(
            reply["nextStep"],
            "Call list_plugins and retry with an exact entryId."
        );
        assert_eq!(std::fs::read_to_string(&paths.patch).expect("read"), text);

        // An id outside the list view refuses without touching the file.
        std::fs::write(&paths.patch, "- id: web\n  disabled: true\n").expect("reset");
        let reply = set_plugin(&paths, "web", false).expect("name-less winner");
        assert_eq!(reply["error"]["code"], "unknown-plugin");
    }

    /// The implied no-op arms: an append that would restate the
    /// composed literal is skipped, and an only-switch override whose
    /// removal leaves the implied literal is deleted.
    #[test]
    fn set_plugin_implied_arms() {
        let scratch = Scratch::new("implied");
        let paths = scratch.paths();
        // The lone disabled row implies `disabled` already: appending
        // `disabled: false` for it is a no-op (the last row wins the
        // literal, so an append that repeats it changes nothing).
        std::fs::write(&paths.patch, "- id: web\n  disabled: true\n").expect("writes");
        let changed = write_plugin_enabled(&paths.patch, "web", "", false).expect("implied write");
        assert!(!changed, "the restated literal is a no-op");
        assert_eq!(
            std::fs::read_to_string(&paths.patch).expect("read"),
            "- id: web\n  disabled: true\n"
        );

        // Two switch rows: overriding the last one back to the implied
        // literal removes the redundant override.
        std::fs::write(
            &paths.patch,
            "- id: web\n  disabled: false\n- id: web\n  disabled: true\n",
        )
        .expect("writes");
        let changed = write_plugin_enabled(&paths.patch, "web", "", true).expect("remove write");
        assert!(changed);
        assert_eq!(
            std::fs::read_to_string(&paths.patch).expect("read"),
            "- id: web\n  disabled: false\n"
        );
    }

    /// The set-bundle arm: a read failure lands in the failed shape, an
    /// enable always refuses through not-bundle, and deselecting a
    /// selected name edits the manifest.
    #[test]
    fn set_bundle_refuses_and_edits_the_selection() {
        let scratch = Scratch::new("bundle");
        let paths = scratch.paths();

        // The missing manifest read failure shape.
        let reply = set_bundle(&paths, "headless", true).expect("missing manifest");
        assert_eq!(reply["application"], "failed");
        assert_eq!(reply["error"]["code"], "operation-error");
        assert!(
            reply["error"]["diagnostic"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );

        // The selection edit needs a manifest.
        std::fs::write(&paths.manifest, "bundles:\n - headless\n").expect("manifest writes");
        let reply = set_bundle(&paths, "headless", true).expect("enable");
        assert_eq!(reply["application"], "failed");
        assert_eq!(reply["error"]["code"], "not-bundle");
        assert_eq!(
            reply["nextStep"],
            "The package declares no fr.bundle and cannot be managed as a plugin; install an \
fr plugin bundle instead."
        );

        // Disabling an unselected name refuses the same way.
        let reply = set_bundle(&paths, "other", false).expect("unselected disable");
        assert_eq!(reply["error"]["code"], "not-bundle");

        // Deselecting the selected name edits the manifest.
        let reply = set_bundle(&paths, "headless", false).expect("deselect");
        assert_eq!(reply["changed"], json!(true));
        assert_eq!(reply["application"], "restart-required");
        let text = std::fs::read_to_string(&paths.manifest).expect("read");
        assert!(!text.contains("headless"), "selection updated: {text}");
    }

    /// The carrierless actions refuse through the adjudicated shapes:
    /// the removal through not-removable, the install and the
    /// exemption write through the operation-error diagnostics, and
    /// the exemption read through a tool error.
    #[test]
    fn carrierless_actions_refuse_in_shape() {
        let removed = remove_bundle("headless");
        assert_eq!(removed["application"], "failed");
        assert_eq!(removed["stage"], "remove");
        assert_eq!(removed["error"]["code"], "not-removable");
        assert_eq!(
            removed["nextStep"],
            "Call list_bundles; only bundles marked removable can be removed."
        );

        let installed = install_bundle(&json!({ "target": "npm:pkg" })).expect("install");
        assert_eq!(installed["application"], "failed");
        assert_eq!(installed["stage"], "install");
        assert_eq!(installed["enabled"], json!(true));
        assert_eq!(
            installed["error"]["diagnostic"],
            "bundle installation has no carrier in fr: plugins are static Rust artifacts \
assembled at startup, with no npm runtime, no package registry, and no live reload"
        );
        let disabled_install =
            install_bundle(&json!({ "target": "npm:pkg", "enabled": false })).expect("install");
        assert_eq!(disabled_install["enabled"], json!(false));

        let exempt = set_version_exemption(&json!({
            "target": "a@1",
            "runtimeVersion": "0.1.0",
            "enabled": true,
        }))
        .expect("exemption");
        assert_eq!(exempt["application"], "failed");
        assert_eq!(exempt["error"]["code"], "operation-error");
        assert_eq!(
            exempt["error"]["diagnostic"],
            "version exemptions have no carrier in fr: plugins are static Rust artifacts with \
no runtime compatibility machinery"
        );

        let error = list_version_exemptions().unwrap_err();
        assert_eq!(
            error.message,
            "version exemptions have no carrier in fr: there is no runtime compatibility \
machinery to query"
        );
    }

    /// The escalation preamble runs before any file touch: a refusing
    /// approver stops the call with the profile tree untouched.
    #[test]
    fn approval_gate_runs_before_any_file_touch() {
        use crate::tools::{ApprovalSeam, PolicySeam};
        struct Frozen;
        impl PolicySeam for Frozen {
            fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
                Ok(Some(json!({ "mode": "read-only" })))
            }
        }
        struct Refuser;
        impl ApprovalSeam for Refuser {
            fn request(
                &self,
                _ask: &fr_sandbox::escalation::EscalationAsk,
            ) -> Result<String, String> {
                Err("denied".to_owned())
            }
        }
        let scratch = Scratch::new("approval");
        let paths = scratch.paths();
        std::fs::write(&paths.patch, "[]\n").expect("writes");
        let error = execute(
            &Frozen,
            &Refuser,
            &json!({ "action": "list_plugins" }),
            None,
            "call-1",
        )
        .unwrap_err();
        assert_ne!(error.message, String::new());
        assert_eq!(
            std::fs::read_to_string(&paths.patch).expect("read"),
            "[]\n",
            "the refusal precedes any file touch"
        );
    }

    /// The profile gate carries the fr-authored text, and the
    /// presentation pair pins the dsh shapes.
    #[test]
    fn profile_gate_and_presentation_pin_the_texts() {
        assert_eq!(
            PROFILE_REQUIRED,
            "plugin_manager requires an active profile: set FR_PROFILE"
        );
        let arguments = json!({ "action": "list_plugins" });
        assert_eq!(
            present_call(&arguments),
            json!({
                "card": "generic",
                "title": "Manage profile plugins",
                "kind": "read",
                "rawInput": arguments,
            })
        );
        assert_eq!(
            present_call(&json!({ "action": "set_plugin" }))["kind"],
            json!("other")
        );
        let rendered = render(&json!({}), &json!("[]"));
        assert_eq!(rendered, json!([{ "type": "text", "text": "[]" }]));
    }
}
