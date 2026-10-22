//! Offline config dump (PRD 6.3; 2026-10-22 Q18-Q24 rulings).
//!
//! `render_dump` composes the assembly without booting: the base root file
//! plus every layer's raw patch rows go through ONE flattened
//! `apply_entry_patches` call per layer prefix — the exact arguments the
//! boot's single composition passes for that prefix — and the result renders
//! as YAML in the entry-list dialect with `!!js` expressions printed
//! verbatim, unevaluated. The layer files use a shape-only parse (top-level
//! sequence, each entry a mapping; any keys are kept), mirroring dsh's
//! structural-trust dump tool: the dump prints whatever the file holds even
//! when the boot-grade parse would reject it, and `render_dump` output
//! re-parses to the same composition the boot mounts (Q24A).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::patch::parse_js_dialect;
use crate::stack::{
    EntryKind, LoadedProfile, PATCH_FILENAME, anchor_base, builtin_bundle_yaml, join_lexical,
    json_quote,
};
use regex::Regex;
use serde_yaml_ng::Value;

/// One overlay patch list with the source label printed in dump comments
/// (dsh `ConfigDumpLayer`).
#[derive(Debug, Clone)]
pub struct DumpLayer {
    /// Source name shown in dump comments (a bundle name or file path).
    pub label: String,
    /// The layer's raw patch rows (shape-only parse; `!!js` preserved).
    pub patches: Vec<Value>,
}

/// Reads dump layers in entry-bundle, profile-bundle, profile-patch, home,
/// then argv order without composing them (dsh `collectConfigDumpLayers`).
///
/// `default_only` omits the profile, home, and argv layers without reading
/// their files — the recovery diagnostic that dumps a broken profile (Q19A).
///
/// # Errors
///
/// Layer files that exist but cannot be read or parsed are fail-closed with
/// the dsh dump diagnostics; overlay absence is fatal (the caller named the
/// file).
pub fn collect_dump_layers(
    home: &Path,
    entry: EntryKind,
    loaded: &LoadedProfile,
    default_only: bool,
    overlays: &[PathBuf],
    bin: &str,
) -> Result<Vec<DumpLayer>> {
    let mut layers: Vec<DumpLayer> = Vec::new();
    // Layer 1: the entry default bundle (fr mounts it ahead of the manifest
    // bundle stack, so the dump must too — otherwise the dump could not equal
    // the boot mount). Built-in bundles are embedded constants, not files:
    // the boot parse keeps their names literal, so the raw re-parse must not
    // anchor relative names to the cwd either.
    let entry_yaml = builtin_bundle_yaml(entry.bundle_name());
    if let Some(yaml) = entry_yaml {
        layers.push(DumpLayer {
            label: entry.bundle_name().to_owned(),
            patches: parse_raw_patch_list(
                yaml,
                Path::new(entry.bundle_name()),
                "patches",
                bin,
                false,
            )?,
        });
    }
    for name in &loaded.bundle_names {
        // `bundle_names` records only manifest names that resolved to
        // built-ins, so this lookup always succeeds.
        let Some(yaml) = builtin_bundle_yaml(name) else {
            continue;
        };
        layers.push(DumpLayer {
            label: name.clone(),
            patches: parse_raw_patch_list(yaml, Path::new(name), "patches", bin, false)?,
        });
    }
    if !default_only {
        let patch_path = loaded.dir.join(PATCH_FILENAME);
        if patch_path.exists()
            && let Some(patches) = load_raw_optional(&patch_path, bin)?
        {
            layers.push(DumpLayer {
                label: patch_path.display().to_string(),
                patches,
            });
        }
        let home_patch = home.join(PATCH_FILENAME);
        if let Some(patches) = load_raw_optional(&home_patch, bin)? {
            layers.push(DumpLayer {
                label: home_patch.display().to_string(),
                patches,
            });
        }
        for file in overlays {
            let absolute = resolve_path(file);
            layers.push(DumpLayer {
                label: absolute.display().to_string(),
                patches: load_raw_overlay(&absolute, bin)?,
            });
        }
    }
    Ok(layers)
}

/// Composes the configured entry list offline and renders it as a YAML
/// document with source comment separators (dsh `renderConfigDump`,
/// app-boot index.ts:635-; Q18-Q24 rulings).
///
/// One flattened `apply_entry_patches` call per layer prefix, `# ==`
/// separator comments grouping contiguous rows of the same source and
/// patches, `!!js` expressions verbatim, and skipped-patch diagnostics
/// through `warn` with the layer label — mirroring the Loader's boot-time
/// warning.
///
/// # Errors
///
/// [`Error::Io`] when the base file cannot be read,
/// [`Error::PatchParse`] when it is not a valid YAML sequence of entries —
/// the pinned error triple `fr: failed to read config` /
/// `fr: failed to parse config` / `fr: config <path> must be a top-level
/// YAML array of entries`.
pub fn render_dump(
    bin: &str,
    base_path: &Path,
    layers: &[DumpLayer],
    warn: &dyn Fn(&str),
) -> Result<String> {
    let content = fs::read_to_string(base_path).map_err(|e| {
        Error::Io(format!(
            "{bin}: failed to read config {}: {e}",
            base_path.display()
        ))
    })?;
    // The same structure-aware `!!js` dialect parse as every fr parse: the
    // base parses with verbatim string content and fail-closed tag
    // positions (Q25B).
    let doc: Value = parse_js_dialect(&content).map_err(|e| {
        Error::PatchParse(format!(
            "{bin}: failed to parse config {}: {e}",
            base_path.display()
        ))
    })?;
    let Some(rows) = doc.as_sequence() else {
        return Err(Error::PatchParse(format!(
            "{bin}: config {} must be a top-level YAML array of entries",
            base_path.display()
        )));
    };
    // Structural trust: the dump prints whatever the base file holds.
    let base: Vec<Value> = rows.clone();
    let base_label = base_path.file_name().map_or_else(
        || base_path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut origins: Vec<RowOrigin> = base
        .iter()
        .map(|_| RowOrigin {
            origin: base_label.clone(),
            patched_by: Vec::new(),
        })
        .collect();
    let mut previous_canonical: Vec<String> = base.iter().map(canonical_form).collect();
    let mut previous_warnings: Vec<String> = Vec::new();
    let mut composed: Vec<Value> = base.clone();
    // snapshot_k = ONE application of layers 1..k flattened, using the exact
    // arguments the boot's single composition passes for that prefix. Each
    // call re-warns from the base, so a snapshot's warning list extends the
    // previous one and the new tail belongs to the added layer.
    for (idx, layer) in layers.iter().enumerate() {
        let flattened: Vec<Value> = layers[..=idx]
            .iter()
            .flat_map(|dump_layer| dump_layer.patches.iter().cloned())
            .collect();
        let mut warnings = Vec::new();
        composed = apply_entry_patches(&base, &flattened, &mut warnings);
        for line in warnings.iter().skip(previous_warnings.len()) {
            warn(&format!("{bin}: [{}] {}", layer.label, line));
        }
        for (position, row) in composed.iter().enumerate() {
            if position >= previous_canonical.len() {
                origins.push(RowOrigin {
                    origin: layer.label.clone(),
                    patched_by: Vec::new(),
                });
            } else if canonical_form(row) != previous_canonical[position] {
                origins[position].patched_by.push(layer.label.clone());
            }
        }
        previous_canonical = composed.iter().map(canonical_form).collect();
        previous_warnings = warnings;
    }
    grouped_dump(&composed, &origins)
}

/// One row's source record: the file that contributed the row and the layers
/// that changed it afterwards.
struct RowOrigin {
    origin: String,
    patched_by: Vec<String>,
}

/// Renders the composed rows grouped under one source-and-patches comment per
/// contiguous run (dsh `groupedDump`): the output stays one loadable YAML
/// document while showing which section comes from which file.
fn grouped_dump(composed: &[Value], origins: &[RowOrigin]) -> Result<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut group: Vec<Value> = Vec::new();
    for (row, record) in composed.iter().zip(origins.iter()) {
        let label = if record.patched_by.is_empty() {
            record.origin.clone()
        } else {
            format!(
                "{}, patched by {}",
                record.origin,
                record.patched_by.join(", ")
            )
        };
        if current.as_ref() != Some(&label) {
            flush_group(&mut lines, &mut group, current.take())?;
            current = Some(label);
        }
        group.push(row.clone());
    }
    flush_group(&mut lines, &mut group, current.take())?;
    let mut text = lines.join("\n");
    text.push('\n');
    Ok(text)
}

/// Emits one `# ==` comment plus the pending group's YAML; a trailing newline
/// is trimmed so the next comment joins cleanly.
fn flush_group(
    lines: &mut Vec<String>,
    group: &mut Vec<Value>,
    label: Option<String>,
) -> Result<()> {
    let Some(label) = label else {
        return Ok(());
    };
    if group.is_empty() {
        return Ok(());
    }
    lines.push(format!("# == {label}"));
    let group = Value::Sequence(std::mem::take(group));
    let with_tags = serde_yaml_ng::to_string(&group)
        .map_err(|e| Error::PatchParse(format!("dump serialization failed: {e}")))?;
    let without_tags = serde_yaml_ng::to_string(&strip_js_tags(&group))
        .map_err(|e| Error::PatchParse(format!("dump serialization failed: {e}")))?;
    lines.push(denormalize_group(
        with_tags.trim_end(),
        without_tags.trim_end(),
    ));
    Ok(())
}

/// Re-renders the single-bang `!js` tags `serde_yaml_ng` emits as the `!!js`
/// secondary-handle form the input dialect uses (Q18A), without touching
/// tag-shaped text inside string content (Q25B): the group serializes
/// twice, with and without its `js` tags, so every tag line is the ONLY
/// differing line and content lines stay byte-identical.
#[must_use]
fn denormalize_group(with_tags: &str, without_tags: &str) -> String {
    let tagged_lines: Vec<&str> = with_tags.lines().collect();
    let plain_lines: Vec<&str> = without_tags.lines().collect();
    if tagged_lines.len() != plain_lines.len() {
        return denormalize_js_tags(with_tags);
    }
    let mut out = Vec::with_capacity(tagged_lines.len());
    for (tagged, plain) in tagged_lines.iter().zip(plain_lines.iter()) {
        if tagged == plain {
            out.push((*tagged).to_owned());
            continue;
        }
        match flip_tag_line(tagged, plain) {
            Some(line) => out.push(line),
            // The two serializations only ever differ by the stripped tag
            // prefix; anything else falls back to the line-anchored flip.
            None => return denormalize_js_tags(with_tags),
        }
    }
    out.join("\n")
}

/// Grows the stripped `!js` tag prefix back to `!!js` on one differing
/// line; `None` when the line difference is not exactly a removed `!js`
/// tag.
fn flip_tag_line(tagged: &str, plain: &str) -> Option<String> {
    let mut prefix = 0;
    for (ta, tb) in tagged.chars().zip(plain.chars()) {
        if ta != tb {
            break;
        }
        prefix += ta.len_utf8();
    }
    let tagged_rest = tagged.get(prefix..)?;
    let plain_rest = plain.get(prefix..)?;
    let after = tagged_rest.strip_prefix("!js")?;
    // The emitter separates tag and scalar with one space, and the stripped
    // serialization drops that space together with the tag.
    let after = after.strip_prefix(' ').unwrap_or(after);
    (after == plain_rest).then(|| format!("{}!{}", &tagged[..prefix], tagged_rest))
}

/// Clones the value with every `js` tag (and only those) stripped, so the
/// tag-free serialization differs from the tagged one exactly at tag lines.
fn strip_js_tags(value: &Value) -> Value {
    match value {
        Value::Tagged(tagged) if tagged.tag.to_string().trim_start_matches('!') == "js" => {
            strip_js_tags(&tagged.value)
        }
        Value::Tagged(tagged) => Value::Tagged(Box::new(serde_yaml_ng::value::TaggedValue {
            tag: tagged.tag.clone(),
            value: strip_js_tags(&tagged.value),
        })),
        Value::Sequence(items) => Value::Sequence(items.iter().map(strip_js_tags).collect()),
        Value::Mapping(map) => Value::Mapping(
            map.iter()
                .map(|(key, item)| (strip_js_tags(key), strip_js_tags(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Matches the single-bang `!js` tags `serde_yaml_ng` emits at YAML value
/// positions; compiled once. Fallback only: [`denormalize_group`] handles
/// the line-differential flip, and this regex rewrite stays for the
/// defensive case where the two serializations do not line up.
static DENORMALIZE_JS_TAG: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// Line-anchored `!js` -> `!!js` rewrite; see [`DENORMALIZE_JS_TAG`].
#[must_use]
fn denormalize_js_tags(yaml: &str) -> String {
    let value_position = DENORMALIZE_JS_TAG.get_or_init(|| {
        Regex::new(r"(?m)^([ \t]*(?:-[ \t]+)?[A-Za-z_][A-Za-z0-9_-]*:[ \t]+|^[ \t]*-[ \t]+)!js")
            .expect("valid regex")
    });
    value_position.replace_all(yaml, "${1}!!js").into_owned()
}

/// Resolves a `--patch` overlay path to the absolute, lexically normalized
/// form the dump prints in comments (dsh `resolve(file)`).
fn resolve_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return join_lexical(Path::new("/"), path);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    join_lexical(&cwd, path)
}

/// Loads an optional raw patch list: absent = `None`, any other read failure
/// or a shape violation is fatal (dsh `loadOptionalPatches` diagnostics).
fn load_raw_optional(path: &Path, bin: &str) -> Result<Option<Vec<Value>>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(Error::Io(format!(
                "{bin}: failed to read patches {}: {e}",
                path.display()
            )));
        }
    };
    parse_raw_patch_list(&content, path, "patches", bin, true).map(Some)
}

/// Loads a required `--patch` overlay raw: absence is fatal because the
/// caller named this file (dsh `loadOverlayPatches` diagnostics).
fn load_raw_overlay(path: &Path, bin: &str) -> Result<Vec<Value>> {
    let content = fs::read_to_string(path).map_err(|e| {
        Error::Io(format!(
            "{bin}: failed to read overlay {}: {e}",
            path.display()
        ))
    })?;
    parse_raw_patch_list(&content, path, "overlay", bin, true)
}

/// Parses one raw patch list with shape-only validation (dsh
/// `parsePatchList`): a top-level YAML sequence whose every entry is a
/// mapping, any keys kept, `!!js` preserved unevaluated (structural trust —
/// the dump prints whatever the file holds, Q24A). Insert-row relative names
/// anchor to the file's directory (Q6) when `anchor_relative` holds; built-in
/// bundle layers pass `false` because they are embedded constants with no
/// file, and the boot-grade parse keeps their names literal (PRD 6.2 anchors
/// file layers only).
///
/// # Errors
///
/// [`Error::PatchParse`] with the dsh dump diagnostics for YAML, shape, or
/// row-mapping violations; flow-style `!!js` tags are fail-closed (fr parse
/// dialect).
fn parse_raw_patch_list(
    content: &str,
    path: &Path,
    label: &str,
    bin: &str,
    anchor_relative: bool,
) -> Result<Vec<Value>> {
    let doc: Value = parse_js_dialect(content).map_err(|e| {
        Error::PatchParse(format!(
            "{bin}: failed to parse {label} {}: {e}",
            path.display()
        ))
    })?;
    let Some(rows) = doc.as_sequence() else {
        return Err(Error::PatchParse(format!(
            "{bin}: {label} {} must be a top-level YAML array of loader patch entries",
            path.display()
        )));
    };
    for (index, row) in rows.iter().enumerate() {
        if !row.is_mapping() {
            return Err(Error::PatchParse(format!(
                "{bin}: {label} entry {} in {} must be a mapping (a loader patch entry)",
                index + 1,
                path.display()
            )));
        }
    }
    let mut owned: Vec<Value> = rows.clone();
    if anchor_relative {
        anchor_insert_names_raw(&mut owned, &anchor_base(path));
    }
    Ok(owned)
}

/// Anchors `./`/`../` insert-row names to the patch file's directory on raw
/// rows (same rule as the boot-grade anchoring; Q6). Override-row names are
/// matchers and never touched.
fn anchor_insert_names_raw(rows: &mut [Value], base: &Path) {
    let name_key = Value::String("name".into());
    let insert_key = Value::String("insert".into());
    for row in rows.iter_mut() {
        let Some(mapping) = row.as_mapping_mut() else {
            continue;
        };
        let Some(Value::Sequence(inserted)) = mapping.get_mut(&insert_key) else {
            continue;
        };
        for inserted_row in inserted.iter_mut() {
            let Some(inner) = inserted_row.as_mapping_mut() else {
                continue;
            };
            let anchored = match inner.get(&name_key) {
                Some(Value::String(name)) => name.strip_prefix("./").map_or_else(
                    || {
                        name.starts_with("../")
                            .then(|| join_lexical(base, Path::new(name)).display().to_string())
                    },
                    |rest| Some(join_lexical(base, Path::new(rest)).display().to_string()),
                ),
                _ => None,
            };
            if let Some(anchored) = anchored {
                inner.insert(name_key.clone(), Value::String(anchored));
            }
        }
    }
}

/// Path to one composed row: indices from the root list down through group
/// `config` arrays (`[root]` for a root row, `[root, child]` for a group
/// child). Root rows are never removed or reordered, so their paths stay
/// valid; a group `config` replacement cuts the old children out of the tree,
/// which [`detach_descendants`] handles by freezing their index records.
type RowPath = Vec<usize>;

/// One id-index record ([`build_map`]): either a live row addressable by
/// path, or a frozen snapshot of a row detached from the tree by an ancestor
/// `config` replacement. dsh's index holds direct object references, so
/// patches keep applying to detached rows with invisible effect — the
/// snapshot reproduces exactly that: their warnings still fire (the name
/// and group checks read the frozen row), their changes never show.
#[derive(Clone, Debug)]
enum RowTarget {
    /// Path from the root list down to the live row.
    Live(RowPath),
    /// The row's last state before an ancestor `config` replacement cut it
    /// out of the tree.
    Detached(Value),
}

/// JS truthiness for raw YAML values (the patch algorithm branches on truthy
/// checks, not presence).
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Bool(true) | Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => true,
        // JS 0 and NaN are falsy; any other number is truthy.
        Value::Number(number) => number.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(text) => !text.is_empty(),
    }
}

/// Indexes rows by `id` (dsh `buildMap`): only non-empty string ids map, and
/// the recursion descends into a truthy-`group` row's array `config`, so a
/// group child is addressable exactly like a root row. Last same-id row wins
/// (JS `Map.set` overwrite).
fn build_map(
    rows: &[Value],
    parent: &[usize],
    offset: usize,
    map: &mut HashMap<String, RowTarget>,
) {
    let id_key = Value::String("id".into());
    let group_key = Value::String("group".into());
    let config_key = Value::String("config".into());
    for (position, row) in rows.iter().enumerate() {
        let mut path = parent.to_vec();
        path.push(offset + position);
        let Some(mapping) = row.as_mapping() else {
            continue;
        };
        if let Some(Value::String(id)) = mapping.get(&id_key)
            && !id.is_empty()
        {
            map.insert(id.clone(), RowTarget::Live(path.clone()));
        }
        if mapping.get(&group_key).is_some_and(js_truthy)
            && let Some(Value::Sequence(children)) = mapping.get(&config_key)
        {
            build_map(children, &path, 0, map);
        }
    }
}

/// Walks a recorded [`RowPath`] to the mutable row it addresses.
fn row_at_mut<'a>(rows: &'a mut [Value], path: &[usize]) -> Option<&'a mut Value> {
    let (&head, tail) = path.split_first()?;
    let row = rows.get_mut(head)?;
    if tail.is_empty() {
        return Some(row);
    }
    let config = row
        .as_mapping_mut()?
        .get_mut(Value::String("config".into()))?;
    row_at_mut(config.as_sequence_mut()?, tail)
}

/// Handles one truthy-id insert targeting a group entry (dsh insert branch
/// with an id): pushes `inserted` into the target group's array `config` and
/// indexes the added rows so later patches in the same flattened list can
/// target them. Skips with the dsh warning when the target is not a group.
fn insert_into_group(
    data: &mut [Value],
    index: &mut HashMap<String, RowTarget>,
    path: &RowPath,
    inserted: &[Value],
    id_text: &str,
    warnings: &mut Vec<String>,
) {
    let Some(target) = row_at_mut(data, path) else {
        // Index entries always point at live rows.
        return;
    };
    let Some(target_map) = target.as_mapping_mut() else {
        // Indexed rows always carry an id, hence are mappings.
        return;
    };
    let group_key = Value::String("group".into());
    let config_key = Value::String("config".into());
    if !target_map.get(&group_key).is_some_and(js_truthy) {
        warnings.push(format!("patch insert: entry {id_text} is not a group"));
        return;
    }
    if !target_map.get(&config_key).is_some_and(Value::is_sequence) {
        target_map.insert(config_key.clone(), Value::Sequence(Vec::new()));
    }
    let Some(config_seq) = target_map
        .get_mut(&config_key)
        .and_then(Value::as_sequence_mut)
    else {
        return;
    };
    let offset = config_seq.len();
    config_seq.extend(inserted.iter().cloned());
    build_map(inserted, path, offset, index);
}

/// The reserved row keys the patch algorithm branches on, built once per
/// composition call and shared by the per-patch handlers.
struct PatchKeys {
    /// The row id patch rows target.
    id: Value,
    /// The insert-list key on a patch row.
    insert: Value,
    /// The matcher-only name key.
    name: Value,
    /// The group flag key on a target row.
    group: Value,
    /// The child-list key on a target row.
    config: Value,
}

impl PatchKeys {
    fn new() -> Self {
        Self {
            id: Value::String("id".into()),
            insert: Value::String("insert".into()),
            name: Value::String("name".into()),
            group: Value::String("group".into()),
            config: Value::String("config".into()),
        }
    }
}

/// Applies patch lists to an entry list — THE patch semantics of the dump,
/// a verbatim port of dsh `applyEntryPatches`
/// (vendor/include/src/index.ts:57-127; Q24A). The input is never mutated:
/// the dump re-runs the composition per layer prefix, so shared state must
/// never leak between snapshots. Inserted rows are indexed as they are added,
/// so a later patch in the same flattened list can target a row an earlier
/// patch inserted. A patch that matches nothing records a warning and is
/// skipped, with the exact dsh wording (`%C` renders as `JSON.stringify`).
///
/// dsh's id index holds direct object references, so a plain `config`
/// override on a group swaps the subtree while later patches still reach the
/// old children through their references — mutating them invisibly. The path
/// index mirrors that with [`RowTarget::Detached`] snapshots: a replaced
/// group's old children freeze, their warnings keep reading the frozen row,
/// and their patches change nothing.
fn apply_entry_patches(
    base: &[Value],
    patches: &[Value],
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    if patches.is_empty() {
        return base.to_vec();
    }
    let mut data: Vec<Value> = base.to_vec();
    let mut index: HashMap<String, RowTarget> = HashMap::new();
    build_map(&data, &[], 0, &mut index);
    let keys = PatchKeys::new();
    for patch in patches {
        // parse_raw_patch_list guarantees mapping rows.
        let Some(mapping) = patch.as_mapping() else {
            continue;
        };
        let insert = mapping.get(&keys.insert).filter(|value| js_truthy(value));
        let id = mapping.get(&keys.id);
        let name = mapping.get(&keys.name);
        if let Some(Value::Sequence(inserted)) = insert {
            apply_insert_patch(&mut data, &mut index, id, inserted, &keys, warnings);
        } else {
            apply_override_patch(&mut data, &mut index, id, name, mapping, &keys, warnings);
        }
    }
    data
}

/// Handles one truthy-`insert` patch row: an id-targeted group insert, or a
/// root-level append when the id is absent or falsy (dsh insert branch).
fn apply_insert_patch(
    data: &mut Vec<Value>,
    index: &mut HashMap<String, RowTarget>,
    id: Option<&Value>,
    inserted: &[Value],
    keys: &PatchKeys,
    warnings: &mut Vec<String>,
) {
    if id.is_none_or(|value| !js_truthy(value)) {
        // Root-level insert; rows are cloned so the patch list itself is
        // never mutated (dsh structuredClone's the whole list instead).
        let offset = data.len();
        data.extend(inserted.iter().cloned());
        build_map(inserted, &[], offset, index);
        return;
    }
    let id_text = id.map_or_else(String::new, canonical_form);
    let located = match id {
        // Only string ids can match the string-keyed index
        // (dsh `entryMap.get(id)` on a string-keyed Map).
        Some(Value::String(target_id)) => index.get(target_id).cloned(),
        _ => None,
    };
    match located {
        Some(RowTarget::Live(path)) => {
            insert_into_group(data, index, &path, inserted, &id_text, warnings);
        }
        // dsh pushes into the detached group's config array; the change is
        // invisible, but the not-a-group check still reads the detached row.
        // (Rows inserted there are likewise invisible and are not indexed.)
        Some(RowTarget::Detached(target)) => {
            if !target
                .as_mapping()
                .and_then(|map| map.get(&keys.group))
                .is_some_and(js_truthy)
            {
                warnings.push(format!("patch insert: entry {id_text} is not a group"));
            }
        }
        None => warnings.push(format!("patch insert: entry {id_text} not found")),
    }
}

/// Handles one non-insert patch row: the per-key overrides against the id's
/// live row, or the invisible name-checked no-op against a detached snapshot
/// (dsh keeps mutating the detached object its reference reaches).
fn apply_override_patch(
    data: &mut [Value],
    index: &mut HashMap<String, RowTarget>,
    id: Option<&Value>,
    name: Option<&Value>,
    mapping: &serde_yaml_ng::Mapping,
    keys: &PatchKeys,
    warnings: &mut Vec<String>,
) {
    if id.is_none_or(|value| !js_truthy(value)) {
        warnings.push("patch: id is required for non-insert patches".to_owned());
        return;
    }
    let id_text = id.map_or_else(String::new, canonical_form);
    let located = match id {
        Some(Value::String(target_id)) => index.get(target_id).cloned(),
        _ => None,
    };
    match located {
        Some(RowTarget::Live(path)) => {
            // `name` is a matcher only and is never written (dsh destructures
            // it out of the overrides); a truthy mismatch against the target's
            // name warns and skips. A target without a name renders as
            // `undefined`, matching `JSON.stringify(undefined)`.
            if let Some(name_value) = name.filter(|value| js_truthy(value)) {
                let expected = row_at_mut(data, &path)
                    .and_then(|target| target.as_mapping().map(|m| m.get(&keys.name)))
                    .flatten()
                    .map_or_else(|| "undefined".to_owned(), canonical_form);
                if canonical_form(name_value) != expected {
                    warnings.push(format!(
                        "patch: name mismatch for {id_text} (expected {expected}, got {}), skipping",
                        canonical_form(name_value)
                    ));
                    return;
                }
            }
            let Some(target) = row_at_mut(data, &path) else {
                return;
            };
            let Some(target_map) = target.as_mapping_mut() else {
                return;
            };
            // A plain `config` override swaps the whole subtree: snapshot the
            // old children before the write so their ids can detach (dsh
            // keeps reaching them through references).
            let old_config = if mapping.keys().any(|key| key.as_str() == Some("config")) {
                target_map.get(&keys.config).cloned()
            } else {
                None
            };
            // Every remaining key overrides per key (dsh `Object.entries`
            // order; serde_yaml_ng's ordered mapping keeps the same insertion
            // order and replaces existing values in place). Non-string keys
            // cannot occur in the JS semantics the port mirrors and are
            // skipped.
            for (key, value) in mapping {
                match key.as_str() {
                    Some("id" | "insert" | "name") | None => {}
                    Some(_) => {
                        target_map.insert(key.clone(), value.clone());
                    }
                }
            }
            if let Some(old) = old_config {
                detach_descendants(index, &path, &old);
            }
        }
        // The name check still reads the detached row (dsh compares against
        // the object its reference reaches); a matching patch mutates the
        // detached object — invisible here.
        Some(RowTarget::Detached(snapshot)) => {
            if let Some(name_value) = name.filter(|value| js_truthy(value)) {
                let expected = snapshot
                    .as_mapping()
                    .and_then(|map| map.get(&keys.name))
                    .map_or_else(|| "undefined".to_owned(), canonical_form);
                if canonical_form(name_value) != expected {
                    warnings.push(format!(
                        "patch: name mismatch for {id_text} (expected {expected}, got {}), skipping",
                        canonical_form(name_value)
                    ));
                }
            }
        }
        None => warnings.push(format!("patch: entry {id_text} not found")),
    }
}

/// Freezes every indexed row living under a group whose `config` a patch just
/// replaced: dsh's reference index still reaches the old children, so later
/// patches mutate the detached objects instead of whatever now occupies their
/// old positions. Each record becomes a [`RowTarget::Detached`] snapshot of
/// the row's state at detachment time.
fn detach_descendants(index: &mut HashMap<String, RowTarget>, group: &[usize], old_config: &Value) {
    let null = Value::Null;
    for entry in index.values_mut() {
        let RowTarget::Live(path) = entry else {
            continue;
        };
        if path.len() <= group.len() || !path.starts_with(group) {
            continue;
        }
        // Walk the old subtree along the recorded path to snapshot the row as
        // it was when the replacement cut it out; the indices were valid in
        // that subtree, so only the `null` fallback is unreachable in practice.
        let mut node = old_config;
        for &position in &path[group.len()..] {
            let Some(child) = node.as_sequence().and_then(|seq| seq.get(position)) else {
                node = &null;
                break;
            };
            node = child;
        }
        *entry = RowTarget::Detached(node.clone());
    }
}

/// Serializes a raw YAML value into a change-detection form that
/// distinguishes everything JS `JSON.stringify` distinguishes across
/// snapshots, plus the tagged/untagged distinction the dump needs (dsh
/// compares `JSON.stringify` of entry objects; `!!js` scalars arrive there
/// wrapped, so a tag flip is a row change).
fn canonical_form(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            let _ = write!(out, "{number}");
        }
        Value::String(text) => out.push_str(&json_quote(text)),
        Value::Tagged(tagged) => {
            out.push_str("{\"__tag\":");
            out.push_str(&json_quote(&tagged.tag.to_string()));
            out.push_str(",\"__value\":");
            write_canonical(&tagged.value, out);
            out.push('}');
        }
        Value::Sequence(items) => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Mapping(map) => {
            out.push('{');
            for (position, (key, item)) in map.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                write_canonical(key, out);
                out.push(':');
                write_canonical(item, out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BIN: &str = "fr-test-bin";

    /// Creates a unique temp directory; removes it on drop.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("fr-config-dump-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
            Self { dir }
        }

        fn path(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn no_warnings(_: &str) {}

    fn write_file(path: &Path, content: &str) {
        fs::write(path, content).unwrap_or_else(|e| panic!("write {} failed: {e}", path.display()));
    }

    /// Parses a raw patch-list fixture into its rows (the shape the dump
    /// layers carry).
    fn raw_rows(text: &str) -> Vec<Value> {
        match parse_js_dialect(text) {
            Ok(Value::Sequence(rows)) => rows,
            other => panic!("fixture must parse to a sequence: {other:?}"),
        }
    }

    /// The dsh spec's base fixture: two rows, one carrying a `!!js` config
    /// value.
    fn write_base(dir: &Path) -> PathBuf {
        let base = dir.join("base.yml");
        write_file(
            &base,
            "- id: shared\n  name: ./noop.so\n  config:\n    value: base\n    key: !!js process.env.FR_DUMP_SPEC\n- id: untouched\n  name: ./noop.so\n",
        );
        base
    }

    #[test]
    fn composes_layers_prints_verbatim_js_and_labels_sections() {
        let dir = TempDir::new("labels");
        let base = write_base(dir.path());
        let surface = dir.path().join("surface.yml");
        write_file(
            &surface,
            "- id: shared\n  config:\n    value: surface\n    key: !!js process.env.FR_DUMP_SPEC\n- insert:\n    - id: surface-extra\n      name: ./noop.so\n",
        );
        let user = dir.path().join("user.yml");
        write_file(&user, "- id: surface-extra\n  config:\n    value: user\n");
        let surface_rows = load_raw_overlay(&surface, BIN).unwrap_or_else(|e| panic!("{e}"));
        let user_rows = load_raw_overlay(&user, BIN).unwrap_or_else(|e| panic!("{e}"));
        let dump = render_dump(
            BIN,
            &base,
            &[
                DumpLayer {
                    label: "surface.yml".to_owned(),
                    patches: surface_rows,
                },
                DumpLayer {
                    label: "user.yml".to_owned(),
                    patches: user_rows,
                },
            ],
            &no_warnings,
        )
        .unwrap_or_else(|e| panic!("dump failed: {e}"));
        // Comments do not break loadability: the dump parses as one document
        // equal to what the boot composition produces.
        let parsed: Value =
            parse_js_dialect(&dump).unwrap_or_else(|e| panic!("dump must re-parse: {e}"));
        let rows = parsed.as_sequence().expect("sequence");
        assert_eq!(rows.len(), 3, "{parsed:?}");
        assert_eq!(rows[0]["id"], "shared");
        assert_eq!(rows[0]["config"]["value"], "surface");
        assert!(
            matches!(rows[0]["config"]["key"], Value::Tagged(_)),
            "!!js round-trips as a tagged scalar: {:?}",
            rows[0]["config"]["key"]
        );
        assert_eq!(rows[1]["id"], "untouched");
        assert_eq!(rows[2]["id"], "surface-extra");
        // Unevaluated: the expression text round-trips verbatim.
        assert!(
            dump.contains("!!js process.env.FR_DUMP_SPEC"),
            "verbatim !!js: {dump}"
        );
        // Source separators: origin file, plus every layer that changed the
        // row; an inserted row carries the inserting layer as its origin.
        assert!(
            dump.contains("# == base.yml, patched by surface.yml"),
            "{dump}"
        );
        assert!(dump.contains("# == base.yml\n- id: untouched"), "{dump}");
        assert!(
            dump.contains("# == surface.yml, patched by user.yml\n- id: surface-extra"),
            "{dump}"
        );
        assert!(
            dump.find("# == base.yml, patched by surface.yml")
                < dump.find("# == base.yml\n- id: untouched"),
            "section order: {dump}"
        );
    }

    #[test]
    fn groups_contiguous_same_origin_under_one_separator() {
        let dir = TempDir::new("grouping");
        let base = dir.path().join("base.yml");
        write_file(
            &base,
            "- id: a\n  name: ./noop.so\n- id: b\n  name: ./noop.so\n",
        );
        let dump = render_dump(BIN, &base, &[], &no_warnings)
            .unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert_eq!(
            dump.matches("# == base.yml").count(),
            1,
            "one separator for one contiguous run: {dump}"
        );
        assert!(dump.contains("# == base.yml\n- id: a"), "{dump}");
    }

    #[test]
    fn single_call_flattened_semantics_group_child_not_patched_by_later_layer() {
        // The boot composition flattens every layer into ONE call whose id
        // index sees inserted rows but NOT children introduced by a plain
        // group `config` replacement. A per-layer composition would rebuild
        // the index between layers and let the second layer patch that child
        // — a tree the real boot never mounts. Pin the single-call semantics:
        // the child patch is skipped (with the layer-labeled warning).
        let dir = TempDir::new("single-call");
        let base = dir.path().join("base.yml");
        write_file(
            &base,
            "- id: g\n  name: ./group.so\n  group: true\n  config: []\n",
        );
        let layer_a = DumpLayer {
            label: "a.yml".to_owned(),
            patches: raw_rows(
                "- id: g\n  config:\n    - id: child\n      name: ./noop.so\n      config:\n        v: 1\n",
            ),
        };
        let layer_b = DumpLayer {
            label: "b.yml".to_owned(),
            patches: raw_rows("- id: child\n  config:\n    v: 2\n"),
        };
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let sink = |line: &str| {
            warnings
                .lock()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .push(line.to_owned());
        };
        let dump = render_dump(BIN, &base, &[layer_a, layer_b], &sink)
            .unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert_eq!(
            warnings
                .into_inner()
                .unwrap_or_else(|e| panic!("poisoned: {e}")),
            vec![format!("{BIN}: [b.yml] patch: entry \"child\" not found")],
        );
        let parsed: Value =
            parse_js_dialect(&dump).unwrap_or_else(|e| panic!("dump must re-parse: {e}"));
        assert_eq!(
            parsed[0]["config"][0]["config"]["v"], 1,
            "the skipped layer must not change the row: {parsed:?}"
        );
        // The skipped layer did not change the row, so the comment does not
        // list it.
        assert!(
            dump.contains("# == base.yml, patched by a.yml\n- id: g"),
            "{dump}"
        );
        assert!(!dump.contains("b.yml\n- id: g"), "{dump}");
    }

    #[test]
    fn absent_target_warns_with_layer_label_and_continues() {
        let dir = TempDir::new("absent");
        let base = write_base(dir.path());
        let overlay = dir.path().join("overlay.yml");
        write_file(
            &overlay,
            "- id: only-on-another-surface\n  config:\n    value: ignored\n- id: shared\n  config:\n    value: patched\n",
        );
        let rows = load_raw_overlay(&overlay, BIN).unwrap_or_else(|e| panic!("{e}"));
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let sink = |line: &str| {
            warnings
                .lock()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .push(line.to_owned());
        };
        let dump = render_dump(
            BIN,
            &base,
            &[DumpLayer {
                label: "overlay.yml".to_owned(),
                patches: rows,
            }],
            &sink,
        )
        .unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert_eq!(
            warnings
                .into_inner()
                .unwrap_or_else(|e| panic!("poisoned: {e}")),
            vec![format!(
                "{BIN}: [overlay.yml] patch: entry \"only-on-another-surface\" not found"
            )],
        );
        let parsed: Value =
            parse_js_dialect(&dump).unwrap_or_else(|e| panic!("dump must re-parse: {e}"));
        assert_eq!(parsed[0]["config"]["value"], "patched");
    }

    #[test]
    fn base_config_read_parse_shape_errors() {
        let dir = TempDir::new("base-errors");
        let absent = render_dump(BIN, &dir.path().join("absent.yml"), &[], &no_warnings);
        let err = absent.expect_err("absent base must fail");
        assert!(
            err.to_string()
                .contains(&format!("{BIN}: failed to read config ")),
            "{err}"
        );
        let invalid = dir.path().join("invalid.yml");
        write_file(&invalid, "invalid: [unclosed\n");
        let err =
            render_dump(BIN, &invalid, &[], &no_warnings).expect_err("unparsable base must fail");
        assert!(
            err.to_string()
                .contains(&format!("{BIN}: failed to parse config ")),
            "{err}"
        );
        let scalar = dir.path().join("scalar.yml");
        write_file(&scalar, "id: not-a-list\n");
        let err =
            render_dump(BIN, &scalar, &[], &no_warnings).expect_err("non-array base must fail");
        assert!(
            err.to_string()
                .contains("must be a top-level YAML array of entries"),
            "{err}"
        );
    }

    #[test]
    fn dump_output_reparses_to_boot_mount_equivalent() {
        // The 1:1 anchor (PRD 6.3): the dump re-parses and composes to the
        // same rows the boot mounts from the same arguments.
        let dir = TempDir::new("boot-equiv");
        let home = dir.path().join("home");
        fs::create_dir_all(&home).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        let profile_dir = home.join("profiles").join("headless");
        crate::stack::init_profile(&profile_dir, &[]).unwrap_or_else(|e| panic!("{e}"));
        write_file(
            &profile_dir.join(crate::stack::PATCH_FILENAME),
            "- id: llm-primary\n  config:\n    model: from-profile\n",
        );
        write_file(
            &home.join(crate::stack::PATCH_FILENAME),
            "- id: llm-primary\n  config:\n    model: from-home\n",
        );
        let overlay = dir.path().join("overlay.yml");
        write_file(
            &overlay,
            "- id: llm-primary\n  config:\n    model: from-overlay\n- insert:\n    - id: extra\n      name: fr-extra\n",
        );
        let ctx = crate::cond::EvalContext::neutral();
        let stack = crate::stack::load_stack(
            &home,
            EntryKind::Headless,
            Some("headless"),
            std::slice::from_ref(&overlay),
            None,
            &no_warnings,
            &ctx,
        )
        .unwrap_or_else(|e| panic!("stack failed: {e}"));
        let loaded = crate::stack::load_profile(&home, "headless", true, &ctx)
            .unwrap_or_else(|e| panic!("profile failed: {e}"));
        let layers =
            collect_dump_layers(&home, EntryKind::Headless, &loaded, false, &[overlay], BIN)
                .unwrap_or_else(|e| panic!("collect failed: {e}"));
        let base = loaded.dir.join(crate::stack::ROOT_FILENAME);
        let dump = render_dump(BIN, &base, &layers, &no_warnings)
            .unwrap_or_else(|e| panic!("dump failed: {e}"));
        let reparsed = crate::patch::parse_patch_lines(&dump, &ctx).unwrap_or_else(|e| {
            panic!("dump must re-parse through the boot-grade parser: {e}\n{dump}")
        });
        let boot_mounted = crate::patch::compose(&stack.layers);
        // The dump prints the composed ENTRY list; each re-parsed row is one
        // entry, so the mount equality holds row for row.
        let dump_rows: Vec<crate::patch::FlatRow> = reparsed
            .iter()
            .map(|line| crate::patch::FlatRow {
                id: line.id.clone().unwrap_or_default(),
                name: line.name.clone().unwrap_or_default(),
                config: line.config.clone().unwrap_or(serde_json::Value::Null),
                disabled: line.disabled.unwrap_or(false),
            })
            .collect();
        assert_eq!(
            dump_rows, boot_mounted.rows,
            "dump re-parse must equal the boot mount\nwarnings boot: {:?}\ndump: {dump}",
            boot_mounted.warnings
        );
    }

    #[test]
    fn default_only_collects_bundle_layers_only_and_skips_broken_user_files() {
        // Recovery diagnostic (Q19A): a broken fr.patch.yml or home patch
        // never blocks the default-only dump because those files are not read.
        let dir = TempDir::new("default-only");
        let home = dir.path().join("home");
        let profile_dir = home.join("profiles").join("headless");
        fs::create_dir_all(&profile_dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        write_file(
            &profile_dir.join(crate::stack::MANIFEST_FILENAME),
            "bundles: []\n",
        );
        write_file(
            &profile_dir.join(crate::stack::PATCH_FILENAME),
            "- id: a\n  bogus: 1\n",
        );
        write_file(&home.join(crate::stack::PATCH_FILENAME), "also: [broken\n");
        let ctx = crate::cond::EvalContext::neutral();
        let loaded = crate::stack::load_profile(&home, "headless", false, &ctx)
            .unwrap_or_else(|e| panic!("default-only load failed: {e}"));
        let overlay = dir.path().join("overlay.yml");
        write_file(&overlay, "- id: b\n  disabled: true\n");
        let layers =
            collect_dump_layers(&home, EntryKind::Headless, &loaded, true, &[overlay], BIN)
                .unwrap_or_else(|e| panic!("default-only collect failed: {e}"));
        assert_eq!(layers.len(), 1, "entry bundle only: {layers:?}");
        assert_eq!(layers[0].label, "headless");
        // The M1 bundle is one insert-group document carrying its rows inside.
        assert_eq!(layers[0].patches.len(), 1, "{:?}", layers[0].patches);
        assert!(
            layers[0].patches[0]
                .as_mapping()
                .and_then(|m| m.get(Value::String("insert".into())))
                .and_then(Value::as_sequence)
                .is_some_and(|rows| rows.len() > 1),
            "the bundle's insert rows: {:?}",
            layers[0].patches[0]
        );
        // With the user layer read, the same files are fail-closed.
        let loaded_bad = crate::stack::load_profile(&home, "headless", true, &ctx);
        assert!(loaded_bad.is_err(), "broken user patch must fail the load");
    }

    #[test]
    fn collect_layer_order_and_labels() {
        let dir = TempDir::new("order");
        let home = dir.path().join("home");
        let profile_dir = home.join("profiles").join("work");
        fs::create_dir_all(&profile_dir).unwrap_or_else(|e| panic!("mkdir failed: {e}"));
        write_file(
            &profile_dir.join(crate::stack::MANIFEST_FILENAME),
            "bundles: []\n",
        );
        write_file(
            &profile_dir.join(crate::stack::PATCH_FILENAME),
            "- id: p\n  disabled: true\n",
        );
        write_file(
            &home.join(crate::stack::PATCH_FILENAME),
            "- id: h\n  disabled: true\n",
        );
        let overlay = dir.path().join("rel-overlay.yml");
        write_file(&overlay, "- id: o\n  disabled: true\n");
        let ctx = crate::cond::EvalContext::neutral();
        let loaded = crate::stack::load_profile(&home, "work", true, &ctx)
            .unwrap_or_else(|e| panic!("load failed: {e}"));
        let layers = collect_dump_layers(
            &home,
            EntryKind::Headless,
            &loaded,
            false,
            std::slice::from_ref(&overlay),
            BIN,
        )
        .unwrap_or_else(|e| panic!("collect failed: {e}"));
        let labels: Vec<&str> = layers.iter().map(|layer| layer.label.as_str()).collect();
        assert_eq!(
            labels.len(),
            4,
            "entry + profile + home + overlay: {labels:?}"
        );
        assert_eq!(labels[0], "headless", "entry bundle first");
        assert!(
            layers[1].label.ends_with("fr.patch.yml") && layers[1].label.contains("work"),
            "profile patch labeled by path: {}",
            layers[1].label
        );
        assert!(
            layers[2].label.ends_with("fr.patch.yml") && layers[2].label.contains("home"),
            "home patch labeled by path: {}",
            layers[2].label
        );
        let expected_overlay = resolve_path(&overlay);
        assert_eq!(
            layers[3].label,
            expected_overlay.display().to_string(),
            "overlay labeled by the resolved absolute path"
        );
    }

    #[test]
    fn raw_parse_rejects_non_sequence_and_non_mapping_rows() {
        let dir = TempDir::new("raw-shape");
        let scalar = dir.path().join("scalar.yml");
        write_file(&scalar, "id: not-a-list\n");
        let err = load_raw_overlay(&scalar, BIN).expect_err("non-array must fail");
        assert!(
            err.to_string()
                .contains("must be a top-level YAML array of loader patch entries"),
            "{err}"
        );
        let rows = dir.path().join("rows.yml");
        write_file(&rows, "- id: a\n- just a scalar\n");
        let err = load_raw_overlay(&rows, BIN).expect_err("scalar row must fail");
        assert!(err.to_string().contains("entry 2 in"), "{err}");
        assert!(err.to_string().contains("must be a mapping"), "{err}");
    }

    #[test]
    fn js_tagged_config_values_survive_override_round_trip() {
        // A layer overriding a config that carries `!!js` values must keep
        // the new tagged values raw and the dump must print them verbatim.
        let dir = TempDir::new("tag-override");
        let base = dir.path().join("base.yml");
        write_file(
            &base,
            "- id: llm\n  name: fr-llm\n  config:\n    model: deepseek-flash\n",
        );
        let overlay = dir.path().join("overlay.yml");
        write_file(
            &overlay,
            "- id: llm\n  config:\n    policy: !!js \"(env.FR_X ?? 'ask') == 'ask' ? 'never' : 'ask'\"\n",
        );
        let overlay_rows = load_raw_overlay(&overlay, BIN).unwrap_or_else(|e| panic!("{e}"));
        let dump = render_dump(
            BIN,
            &base,
            &[DumpLayer {
                label: "overlay.yml".to_owned(),
                patches: overlay_rows,
            }],
            &no_warnings,
        )
        .unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert!(
            dump.contains("!!js '(env.FR_X ?? ''ask'') == ''ask'' ? ''never'' : ''ask'''"),
            "expression text verbatim (js-yaml-style single quoting): {dump}"
        );
        // The whole dump still parses through the boot-grade parser and the
        // tagged value evaluates from the override row.
        let ctx = crate::cond::EvalContext::neutral();
        let reparsed = crate::patch::parse_patch_lines(&dump, &ctx)
            .unwrap_or_else(|e| panic!("re-parse failed: {e}\n{dump}"));
        assert_eq!(reparsed.len(), 1, "{dump}");
        assert_eq!(
            reparsed[0].config,
            Some(json!({"policy": "never"})),
            "neutral ctx: FR_X unset means the default branch holds"
        );
    }

    #[test]
    fn config_replacement_detaches_old_children_like_dsh_references() {
        // dsh's id index holds direct object references: after layer A
        // replaces a group's `config` wholesale, layer B patching an old
        // child id mutates the detached object — invisible in the tree. The
        // path index must freeze the detached row instead of patching
        // whatever row now occupies the stale position.
        let dir = TempDir::new("detach");
        let base = dir.path().join("base.yml");
        write_file(
            &base,
            "- id: g\n  name: group.so\n  group: true\n  config:\n    - id: c\n      name: old.so\n      group: true\n",
        );
        let layer_a = DumpLayer {
            label: "a.yml".to_owned(),
            patches: raw_rows(
                "- id: g\n  config:\n    - id: c\n      name: new.so\n      group: true\n",
            ),
        };
        let layer_b = DumpLayer {
            label: "b.yml".to_owned(),
            patches: raw_rows("- id: c\n  config:\n    v: 2\n"),
        };
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let sink = |line: &str| {
            warnings
                .lock()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .push(line.to_owned());
        };
        let dump = render_dump(BIN, &base, &[layer_a, layer_b], &sink)
            .unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert!(
            warnings
                .into_inner()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .is_empty(),
            "dsh mutates the detached child invisibly: no warning"
        );
        assert!(
            !dump.contains("v: 2"),
            "detached patch must not land: {dump}"
        );
        assert!(
            !dump.contains("patched by b.yml"),
            "the detached layer changed no row: {dump}"
        );
        let parsed: Value =
            parse_js_dialect(&dump).unwrap_or_else(|e| panic!("dump must re-parse: {e}"));
        assert_eq!(
            parsed[0]["config"][0]["name"], "new.so",
            "the new occupant stays untouched: {parsed:?}"
        );
    }

    #[test]
    fn detached_row_still_owns_its_name_and_group_checks() {
        // Warnings about a detached row read the frozen snapshot (dsh
        // compares against the object its reference reaches): a matching name
        // mutates invisibly, a mismatching name still warns against the old
        // row's name, and an insert still checks the old row's group flag.
        let dir = TempDir::new("detach-checks");
        let base = dir.path().join("base.yml");
        write_file(
            &base,
            "- id: g\n  name: group.so\n  group: true\n  config:\n    - id: c\n      name: old.so\n      group: true\n",
        );
        let layer_a = DumpLayer {
            label: "a.yml".to_owned(),
            patches: raw_rows(
                "- id: g\n  config:\n    - id: c\n      name: new.so\n      group: true\n",
            ),
        };
        let layer_b = DumpLayer {
            label: "b.yml".to_owned(),
            patches: raw_rows("- id: c\n  name: old.so\n  config:\n    v: 2\n"),
        };
        let layer_c = DumpLayer {
            label: "c.yml".to_owned(),
            patches: raw_rows("- id: c\n  name: wrong.so\n  config:\n    v: 3\n"),
        };
        let layer_d = DumpLayer {
            label: "d.yml".to_owned(),
            patches: raw_rows("- id: c\n  insert:\n    - id: x\n      name: x.so\n"),
        };
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let sink = |line: &str| {
            warnings
                .lock()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .push(line.to_owned());
        };
        let dump = render_dump(BIN, &base, &[layer_a, layer_b, layer_c, layer_d], &sink)
            .unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert_eq!(
            warnings
                .into_inner()
                .unwrap_or_else(|e| panic!("poisoned: {e}")),
            vec![format!(
                "{BIN}: [c.yml] patch: name mismatch for \"c\" (expected \"old.so\", got \"wrong.so\"), skipping"
            )],
            "only the mismatch warns, against the detached row's name"
        );
        assert!(
            !dump.contains("v: 2"),
            "matching name mutates invisibly: {dump}"
        );
        assert!(!dump.contains("v: 3"), "{dump}");
        assert!(
            !dump.contains("x.so"),
            "detached group insert is invisible: {dump}"
        );
    }

    #[test]
    fn builtin_bundle_layers_keep_relative_insert_names_literal() {
        // Boot parses embedded bundles without a file path, so `./`/`../`
        // insert names stay literal (PRD 6.2 anchors file layers only); the
        // dump's raw re-parse must not anchor them to the cwd either.
        let bundle = "- insert:\n    - id: a\n      name: ./x.so\n";
        let rows = parse_raw_patch_list(bundle, Path::new("headless"), "patches", BIN, false)
            .unwrap_or_else(|e| panic!("bundle parse failed: {e}"));
        assert_eq!(
            rows[0]["insert"][0]["name"], "./x.so",
            "bundle names stay literal: {rows:?}"
        );
        let file_rows = parse_raw_patch_list(
            bundle,
            Path::new("/tmp/patches/fr.patch.yml"),
            "overlay",
            BIN,
            true,
        )
        .unwrap_or_else(|e| panic!("file parse failed: {e}"));
        assert_eq!(
            file_rows[0]["insert"][0]["name"], "/tmp/patches/x.so",
            "file layers anchor to their directory: {file_rows:?}"
        );
    }

    #[test]
    fn dump_content_js_shaped_lines_survive_round_trip() {
        // Q25B: string content lines shaped like `key: !js v` stay
        // single-bang while real `!!js` expression lines print double-banged,
        // and the dump re-parses to the composed tree (dump == boot for any
        // string).
        let dir = TempDir::new("content-js");
        let base = dir.path().join("base.yml");
        write_file(
            &base,
            "- id: a\n  name: x.so\n  note: |\n    k: !js v\n- id: b\n  name: y.so\n  policy: !!js \"(env.FR_X ?? 'ask')\"\n",
        );
        let warnings: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let sink = |line: &str| {
            warnings
                .lock()
                .unwrap_or_else(|e| panic!("poisoned: {e}"))
                .push(line.to_owned());
        };
        let dump =
            render_dump(BIN, &base, &[], &sink).unwrap_or_else(|e| panic!("dump failed: {e}"));
        assert!(
            dump.contains("k: !js v\n"),
            "content line stays single-bang: {dump}"
        );
        assert!(
            dump.contains("policy: !!js"),
            "tag line prints double-bang: {dump}"
        );
        assert!(
            !dump.contains("k: !!js v"),
            "content must never flip: {dump}"
        );
        let reparsed: Value =
            parse_js_dialect(&dump).unwrap_or_else(|e| panic!("dump must re-parse: {e}"));
        let base_text =
            std::fs::read_to_string(&base).unwrap_or_else(|e| panic!("read back failed: {e}"));
        let expected: Value =
            parse_js_dialect(&base_text).unwrap_or_else(|e| panic!("base must re-parse: {e}"));
        assert_eq!(reparsed, expected, "dump == boot for any string");
    }
}
