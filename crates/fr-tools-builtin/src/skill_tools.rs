// Rust guideline compliant 2026-11-02
//! The model-facing `skill` loader tool and its pre-step session catalog
//! (dsh `@deepseek-ai/dsh-tool-skill`, 1:1): the tool body, the durable
//! catalog maintenance pass, and the `/name` gesture pass.
//!
//! The tool body and the pre-step run against the same in-process
//! [`SkillRegistry`] (Q93-A: no cross-crate method numbers for the read
//! faces). The pre-step reaches this crate through the `'skills'`
//! service face ([`serve`]): one method
//! (`fr_wire::services::skills::METHOD_PRE_STEP`) that takes the step's
//! message batch plus the session id and returns the transformed batch.
//!
//! The dsh listeners ride the `agent/pre-step` waterfall with the
//! gesture listener registered outermost (registration order: tool,
//! gesture, catalog), so on the waterfall return path the catalog runs
//! first and the gesture injections land last, right before the
//! workspace-instructions fold. The fr [`pre_step`] composes the same
//! order directly: catalog pass, then gesture pass.
//!
//! Structurally absent fr arms (the Q69-B carrier family): the abort
//! signal rides nothing here (the pre-step is a synchronous in-process
//! transform; the tool pipeline owns deadlines), and the visibility
//! gate is the registration-liveness bit ([`SkillRuntime::registered`])
//! instead of a per-step registry probe — the M1 registry refuses
//! duplicate names, `restrict()` is unreachable without scoped
//! contexts, and the only unregister is the teardown disposer that
//! kills this listener too, so every reachable state agrees with the
//! dsh exact-identity gate on the observable behavior (the
//! 2026-11-02 ruling).

#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use fr_wire::content::{ContentBlock, KnownContentBlock, RawObject};
use fr_wire::event::SessionEventData;
use fr_wire::ids::MessageId;
use fr_wire::message::{KnownMessage, Message, MessageSource};
use fr_wire::services::skills::{METHOD_PRE_STEP, SERVICE_NAME};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::session_query::QueryWire;
use crate::skill_runtime::{
    SkillLookupOptions, SkillRegistry, SkillResourceBase, SkillSummary, escape_text,
    is_model_invocable, is_skill_name, is_user_invocable, render_skill_content,
};
use crate::tools::BodyError;

/// The default catalog-description cap (dsh
/// `DEFAULT_CATALOG_DESCRIPTION_MAX_LENGTH`).
pub const DEFAULT_CATALOG_DESCRIPTION_MAX_LENGTH: usize = 500;

/// The baked skill-tool registration gate (the dsh base composition
/// mounts the row unconditionally; the `BAKED_WEB_SEARCH_ENABLED`
/// toggle judgment — the M1 bundle keeps the default on).
pub const BAKED_SKILL_TOOLS: bool = true;

/// The `tool-skill` config cap resolved for the baked composition (the
/// dsh Config default plus the `assertPositiveInteger` gate, text
/// verbatim).
///
/// # Errors
///
/// Below three (the dsh config validation, text verbatim).
pub fn resolve_catalog_description_max_length(value: usize) -> Result<usize, String> {
    if value < 3 {
        return Err(
            "tool-skill: catalogDescriptionMaxLength must be an integer greater than or equal to 3"
                .to_owned(),
        );
    }
    Ok(value)
}

/// One durable catalog entry: the published fact beside the model-facing
/// prose (dsh `SkillCatalogSource['entries']`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// Kebab-case skill name.
    pub name: String,
    /// The normalized, capped routing description.
    pub description: String,
}

/// The baked skill tool runtime: the registry, the resolved catalog cap,
/// the registration-liveness visibility gate, and the message identity
/// generator (the driver's `next_id` judgment shape).
pub struct SkillRuntime {
    /// The single-layer registry (Q93-A: in-process reads).
    pub registry: Arc<SkillRegistry>,
    /// The resolved `catalog-description-max-length` (the dsh
    /// `catalogDescriptionMaxLength`).
    pub catalog_description_max_length: usize,
    /// The toolVisible carrier: set at activation, cleared never (the
    /// 2026-11-02 ruling; see the module docs).
    pub registered: AtomicBool,
    /// The message identity generator for emitted frames (the shared
    /// runtime's face reads from any thread; the dsh `createUserMessage`
    /// judgment).
    pub next_id: std::sync::Arc<dyn Fn() -> MessageId + Send + Sync>,
}

impl std::fmt::Debug for SkillRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillRuntime")
            .field("registry", &self.registry)
            .field(
                "catalog_description_max_length",
                &self.catalog_description_max_length,
            )
            .field("registered", &self.registered)
            .finish_non_exhaustive()
    }
}

impl SkillRuntime {
    /// Marks the `skill` tool registration committed (the assembly step
    /// sets the gate once the registration row exists).
    pub fn mark_registered(&self) {
        self.registered.store(true, Ordering::SeqCst);
    }

    /// The dsh `ctx.tools.get('skill', agent) === skillTool` gate's fr
    /// carrier.
    #[must_use]
    pub fn tool_visible(&self) -> bool {
        self.registered.load(Ordering::SeqCst)
    }
}

/// The pre-step deps the `'skills'` serve face dispatches against (the
/// wire face owns the binding; tests script the reads).
impl std::fmt::Debug for SkillServiceDeps<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillServiceDeps")
            .field("runtime", &self.runtime)
            .finish_non_exhaustive()
    }
}

/// The pre-step deps the `'skills'` serve face dispatches against (the
/// wire face owns the binding; tests script the reads).
pub struct SkillServiceDeps<'a> {
    /// The baked skill tool runtime.
    pub runtime: &'a SkillRuntime,
    /// The composed cross-plugin seam (the working-directory ensure).
    pub seams: &'a dyn crate::tools::Seams,
    /// The session read seam (the session-query wire judgment; the dsh
    /// `agent.session` events scan).
    pub reads: &'a dyn QueryWire,
}

/// Normalizes one catalog description onto the UTF-16 budget (the dsh
/// `catalogDescription`): whitespace runs collapse, the value trims, and
/// an over-budget value clips with a `...` tail.
///
/// The `\s` character classes are the near-identical carriers (the dsh
/// JavaScript class additionally covers U+FEFF and excludes U+0085; the
/// divergence set is unreachable for validated frontmatter prose).
///
/// # Panics
///
/// The static whitespace regex construction.
#[must_use]
pub fn catalog_description(value: &str, max_length: usize) -> String {
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();
    let whitespace = WHITESPACE.get_or_init(|| Regex::new(r"\s+").expect("whitespace regex"));
    let normalized = whitespace.replace_all(value, " ").trim().to_owned();
    if utf16_len(&normalized) <= max_length {
        return normalized;
    }
    format!("{}...", utf16_truncate(&normalized, max_length - 3))
}

/// The UTF-16 code-unit length (the dsh `String.length` judgment, the
/// sre `maybe_truncate` precedent).
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// Clips to at most `max_units` UTF-16 code units at a char boundary.
fn utf16_truncate(value: &str, max_units: usize) -> &str {
    let mut units = 0;
    for (index, ch) in value.char_indices() {
        if units + ch.len_utf16() > max_units {
            return &value[..index];
        }
        units += ch.len_utf16();
    }
    value
}

/// Projects the invocation-neutral summaries onto the durable entry list
/// (the dsh `catalogSourceEntries`).
#[must_use]
pub fn catalog_source_entries(skills: &[SkillSummary], max_length: usize) -> Vec<CatalogEntry> {
    skills
        .iter()
        .map(|skill| CatalogEntry {
            name: skill.name.clone(),
            description: catalog_description(&skill.description, max_length),
        })
        .collect()
}

/// Renders the model-facing catalog rows (the dsh
/// `renderCatalogEntries`): the pseudo-XML escaping belongs to this
/// frame, never to the stored entry facts.
#[must_use]
fn render_catalog_entries(entries: &[CatalogEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| format!("- `{}`: {}", entry.name, escape_text(&entry.description)))
        .collect()
}

/// The catalog identity (the dsh `digestCatalogEntries`): sha256 over
/// the JSON-array-per-entry canonical form joined by newlines, hex.
///
/// The `serde_json` string escaping matches `JSON.stringify`
/// byte-for-byte (the `\b\t\n\f\r` shortcuts plus `\uXXXX` for the
/// remaining control range; non-ASCII stays literal).
///
/// # Panics
///
/// Catalog entry serialization is infallible over string pairs.
#[must_use]
pub fn digest_catalog_entries(entries: &[CatalogEntry]) -> String {
    let canonical = entries
        .iter()
        .map(|entry| {
            serde_json::to_string(&json!([entry.name, entry.description]))
                .expect("catalog entry serialization")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// The session history scan outcome: whether a catalog was ever
/// published and the digest of the most recent visible one (the dsh
/// `catalogHistory` shape).
#[derive(Debug, Default)]
struct CatalogHistory {
    published: bool,
    visible_digest: Option<String>,
}

/// Extracts the producer source object of one message as an owned value
/// (the Known envelope's unknown kinds and the verbatim fallback both
/// land here; known kinds return none — the catalog and gesture scans
/// only ever look for unknown producer kinds).
fn source_value(message: &Message) -> Option<Value> {
    match message {
        Message::Known(boxed) => match boxed.source() {
            MessageSource::Other(source) => Some(Value::Object(source.clone())),
            MessageSource::Known(_) => None,
        },
        Message::Other(raw) => raw.get("source").cloned(),
    }
}

/// Reads one message's producer source `kind` (the dsh
/// `message.source.kind !== 'user'` gate).
fn source_kind(message: &Message) -> Option<String> {
    source_value(message)?
        .get("kind")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Reads the entries of one durable catalog source, or none when the
/// record is not a usable catalog (the dsh `readCatalogEntries`: an
/// unreadable record is treated as not this plugin's catalog, the
/// posture the replaced-content digest had).
#[must_use]
fn read_catalog_entries(source: &Value) -> Option<Vec<CatalogEntry>> {
    let entries = source.get("entries").and_then(Value::as_array)?;
    let mut readable = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry.get("name").and_then(Value::as_str)?;
        if name.is_empty() {
            return None;
        }
        let description = entry.get("description").and_then(Value::as_str)?;
        readable.push(CatalogEntry {
            name: name.to_owned(),
            description: description.to_owned(),
        });
    }
    Some(readable)
}

/// Builds the source object of one durable catalog message (the dsh
/// `source` spread; key order verbatim, `update` present only on
/// replacements).
fn catalog_source(entries: &[CatalogEntry], update: bool) -> Value {
    let mut source = serde_json::Map::new();
    source.insert("kind".to_owned(), json!("skill-catalog"));
    source.insert("form".to_owned(), json!("catalog"));
    if update {
        source.insert("update".to_owned(), json!(true));
    }
    source.insert(
        "entries".to_owned(),
        Value::Array(
            entries
                .iter()
                .map(|entry| {
                    json!({
                        "name": entry.name,
                        "description": entry.description,
                    })
                })
                .collect(),
        ),
    );
    Value::Object(source)
}

/// One user-role message with one text block and the given producer
/// source (the dsh `createUserMessage`; ids ride the runtime's
/// generator).
fn user_message(next_id: &dyn Fn() -> MessageId, text: String, source: &Value) -> Message {
    Message::Known(Box::new(KnownMessage::User {
        id: next_id(),
        content: vec![ContentBlock::Known(KnownContentBlock::Text { text })],
        source: MessageSource::Other(
            source
                .as_object()
                .cloned()
                .expect("catalog source object shape"),
        ),
        extra: RawObject::new(),
    }))
}

/// Renders the session's first catalog publication (the dsh
/// `renderCatalogMessage`).
#[must_use]
fn render_catalog_message(next_id: &dyn Fn() -> MessageId, entries: &[CatalogEntry]) -> Message {
    let mut lines: Vec<String> = vec![
        "<system-reminder>".to_owned(),
        "A skill is a reusable set of task-specific instructions. The following skills are available in this session:".to_owned(),
        String::new(),
        "<available_skills>".to_owned(),
    ];
    lines.extend(render_catalog_entries(entries));
    lines.extend([
        "</available_skills>".to_owned(),
        String::new(),
        "If the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.".to_owned(),
        "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.".to_owned(),
        "</system-reminder>".to_owned(),
    ]);
    user_message(next_id, lines.join("\n"), &catalog_source(entries, false))
}

/// Renders a replacement catalog (the dsh `renderCatalogUpdate`): the
/// availability paragraphs split on whether entries survive.
#[must_use]
fn render_catalog_update(next_id: &dyn Fn() -> MessageId, entries: &[CatalogEntry]) -> Message {
    let availability: [&str; 2] = if entries.is_empty() {
        [
            "No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs.",
            "A user may still invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool for it.",
        ]
    } else {
        [
            "Use only names in this replacement catalog. If the user names a listed skill, or the task clearly matches its description, call the `skill` tool with the exact name before acting.",
            "A user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.",
        ]
    };
    let mut lines: Vec<String> = vec![
        "<system-reminder>".to_owned(),
        "The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:".to_owned(),
        String::new(),
        "<available_skills>".to_owned(),
    ];
    lines.extend(render_catalog_entries(entries));
    lines.extend([
        "</available_skills>".to_owned(),
        String::new(),
        availability[0].to_owned(),
        availability[1].to_owned(),
        "</system-reminder>".to_owned(),
    ]);
    user_message(next_id, lines.join("\n"), &catalog_source(entries, true))
}

/// Reads the session history: whether a catalog was ever published and
/// the digest of the most recent visible one (the dsh `catalogHistory`):
/// the scan walks the log backwards, skips non-catalog and unreadable
/// records, marks `published` on the first readable catalog, and stops
/// at the first readable catalog whose seq sits in the visible surface.
///
/// A missing live log answers the empty history (the read said the id is
/// not live; the safe posture is the initial-publication path).
///
/// # Errors
///
/// The read transport family or the decode/surface-fold refusal.
fn catalog_history(deps: &SkillServiceDeps<'_>, session: &str) -> Result<CatalogHistory, String> {
    let Some(log) = deps.reads.session_read(session)? else {
        return Ok(CatalogHistory::default());
    };
    let log = crate::session_query::decode_log(log)?;
    let folded = fr_session::surface::fold_surface(&log.events)
        .map_err(|error| format!("invalid session surface: {error}"))?;
    let visible: HashSet<u64> = folded.nodes.iter().map(|seq| seq.get()).collect();
    let mut history = CatalogHistory::default();
    for event in log.events.iter().rev() {
        if event.event_type() != fr_wire::event::event_type::USER_MESSAGE {
            continue;
        }
        let SessionEventData::UserMessage(message) = &event.data else {
            continue;
        };
        let Some(source) = source_value(message) else {
            continue;
        };
        if source.get("kind").and_then(Value::as_str) != Some("skill-catalog") {
            continue;
        }
        let Some(entries) = read_catalog_entries(&source) else {
            continue;
        };
        history.published = true;
        if visible.contains(&event.seq.get()) {
            history.visible_digest = Some(digest_catalog_entries(&entries));
            break;
        }
    }
    Ok(history)
}

/// Finds the durable catalog message inside one step batch: the first
/// message whose source is a readable skill-catalog record (the dsh
/// `catalogMessage`).
fn catalog_message(messages: &[Message]) -> Option<(usize, Vec<CatalogEntry>)> {
    messages.iter().enumerate().find_map(|(index, message)| {
        let source = source_value(message)?;
        if source.get("kind").and_then(Value::as_str) != Some("skill-catalog") {
            return None;
        }
        let entries = read_catalog_entries(&source)?;
        Some((index, entries))
    })
}

/// Removes the message at `index` (the dsh id-filter removal; the index
/// comes from [`catalog_message`] on the same slice).
fn remove_at(messages: Vec<Message>, index: usize) -> Vec<Message> {
    let mut messages = messages;
    messages.remove(index);
    messages
}

/// Replaces the message at `index` in place (the dsh id-keyed map; the
/// replacement carries a fresh identity like every emitted frame).
fn replace_at(messages: Vec<Message>, index: usize, replacement: Message) -> Vec<Message> {
    let mut messages = messages;
    messages[index] = replacement;
    messages
}

/// The catalog pass (the dsh catalog listener, index.ts:215-253, 1:1):
/// observe the model-invocable snapshot, compare digests against the
/// session history and the batch's own catalog message, and append,
/// replace, remove, or leave untouched.
///
/// # Errors
///
/// The cwd ensure, the snapshot, or the history-read refusal family.
fn catalog_pass(
    deps: &SkillServiceDeps<'_>,
    session: &str,
    messages: Vec<Message>,
) -> Result<Vec<Message>, String> {
    let snapshot = if deps.runtime.tool_visible() {
        let cwd = deps.seams.working_directory_ensure(session)?;
        Some(
            deps.runtime
                .registry
                .snapshot(&SkillLookupOptions { cwd: Some(cwd) })?,
        )
    } else {
        // The dsh invisible arm: the snapshot is empty and complete.
        None
    };
    let (skills, complete) = snapshot.map_or_else(
        || (Vec::new(), true),
        |snapshot| (snapshot.skills, snapshot.complete),
    );
    if !complete {
        return Ok(messages);
    }
    let skills: Vec<SkillSummary> = skills.into_iter().filter(is_model_invocable).collect();
    let entries = catalog_source_entries(&skills, deps.runtime.catalog_description_max_length);
    let digest = digest_catalog_entries(&entries);
    let history = catalog_history(deps, session)?;
    let existing = catalog_message(&messages);
    if history.visible_digest.as_deref() == Some(digest.as_str()) {
        return Ok(match existing {
            None => messages,
            Some((index, _)) => remove_at(messages, index),
        });
    }
    if existing
        .as_ref()
        .is_some_and(|(_, existing_entries)| digest_catalog_entries(existing_entries) == digest)
    {
        return Ok(messages);
    }
    if !history.published && skills.is_empty() {
        return Ok(match existing {
            None => messages,
            Some((index, _)) => remove_at(messages, index),
        });
    }
    let catalog = if history.published {
        render_catalog_update(&*deps.runtime.next_id, &entries)
    } else {
        render_catalog_message(&*deps.runtime.next_id, &entries)
    };
    Ok(match existing {
        None => {
            let mut messages = messages;
            messages.push(catalog);
            messages
        }
        Some((index, _)) => replace_at(messages, index, catalog),
    })
}

/// The `/name` gesture grammar (the dsh `SKILL_GESTURE`
/// `/(^|\s)\/([a-z0-9]+(?:-[a-z0-9]+)*)(?=\s|$)/g` carrier): the
/// leading-boundary alternative is captured by the pattern, and the
/// trailing lookahead is the post-match boundary check — for this
/// grammar the consumed-span equivalence is exact, because the only
/// region a rejected match consumes (the one leading whitespace char)
/// cannot itself start a nested gesture (the name span admits no
/// whitespace).
const SKILL_GESTURE: &str = r"(?:^|\s)/([a-z0-9]+(?:-[a-z0-9]+)*)";

/// The `/name` gesture tokens from the claimed user messages,
/// deduplicated in first-seen order (the dsh `invokedSkillNames`).
///
/// Every text block of direct user input is scanned; no other source
/// can forge a gesture.
///
/// # Panics
///
/// The static gesture regex construction.
#[must_use]
pub fn invoked_skill_names(messages: &[Message]) -> Vec<String> {
    static GESTURE: OnceLock<Regex> = OnceLock::new();
    let gesture = GESTURE.get_or_init(|| Regex::new(SKILL_GESTURE).expect("gesture regex"));
    let mut names: Vec<String> = Vec::new();
    for message in messages {
        if source_kind(message).as_deref() != Some("user") {
            continue;
        }
        let content = match message {
            Message::Known(boxed) => boxed.content(),
            Message::Other(_) => continue,
        };
        for block in content {
            let ContentBlock::Known(KnownContentBlock::Text { text }) = block else {
                continue;
            };
            for matched in gesture.find_iter(text) {
                let name = matched
                    .as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if name.is_empty() {
                    continue;
                }
                // The dsh trailing lookahead `(?=\s|$)`: the match is a
                // gesture only when the text ends here or the next char
                // is whitespace.
                let rest = &text[matched.end()..];
                let boundary =
                    rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace);
                if boundary && !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// The gesture pass (the dsh gesture listener, index.ts:179-206, 1:1):
/// scan the claimed user messages, load each named skill, and append
/// the rendered `<skill_content>` bodies as injected instruction
/// context. Unknown names and non-user-invocable skills stay plain
/// prose — the gesture was never a claim this boundary recognizes.
///
/// # Errors
///
/// The cwd ensure or the skill load refusal family.
fn gesture_pass(
    deps: &SkillServiceDeps<'_>,
    session: &str,
    mut messages: Vec<Message>,
) -> Result<Vec<Message>, String> {
    let names = invoked_skill_names(&messages);
    if names.is_empty() {
        return Ok(messages);
    }
    let cwd = deps.seams.working_directory_ensure(session)?;
    let lookup = SkillLookupOptions { cwd: Some(cwd) };
    let mut injections = Vec::new();
    for name in names {
        let skill = deps.runtime.registry.get(&name, &lookup)?;
        let Some(skill) = skill else {
            continue;
        };
        if !is_user_invocable(&skill.to_summary()) {
            continue;
        }
        let text = render_skill_content(
            &skill.name,
            &skill.provider,
            skill.resource_base.as_ref(),
            &skill.content,
        );
        let source = json!({
            "kind": "skill-invocation",
            "name": name,
            "form": "instructions",
        });
        injections.push(user_message(&*deps.runtime.next_id, text, &source));
    }
    if injections.is_empty() {
        return Ok(messages);
    }
    messages.extend(injections);
    Ok(messages)
}

/// One pre-step transform (the dsh pre-step waterfall's outer layers,
/// composed): the catalog pass maintains the durable catalog message,
/// then the gesture pass appends the user-invoked skill bodies.
///
/// # Errors
///
/// The passes' refusal families (fail-closed: the serve face maps them
/// onto the wire error, and the driver fails the step boundary like the
/// dsh listener throw).
pub fn pre_step(
    deps: &SkillServiceDeps<'_>,
    session: &str,
    messages: Vec<Message>,
) -> Result<Vec<Message>, String> {
    let messages = catalog_pass(deps, session, messages)?;
    gesture_pass(deps, session, messages)
}

/// Encodes one resource base onto the wire shape (the dsh spread: the
/// camelCase wire keys, protocol exemption).
#[must_use]
pub fn resource_base_value(base: &SkillResourceBase) -> Value {
    match base {
        SkillResourceBase::Directory { path } => json!({
            "kind": "directory",
            "path": path,
        }),
        SkillResourceBase::Url { url } => json!({
            "kind": "url",
            "url": url,
        }),
        SkillResourceBase::Opaque { description } => json!({
            "kind": "opaque",
            "description": description,
        }),
    }
}

/// Decodes one wire resource base (the schema oneOf).
#[must_use]
fn resource_base_from_value(value: &Value) -> Option<SkillResourceBase> {
    match value.get("kind").and_then(Value::as_str)? {
        "directory" => Some(SkillResourceBase::Directory {
            path: value.get("path").and_then(Value::as_str)?.to_owned(),
        }),
        "url" => Some(SkillResourceBase::Url {
            url: value.get("url").and_then(Value::as_str)?.to_owned(),
        }),
        "opaque" => Some(SkillResourceBase::Opaque {
            description: value.get("description").and_then(Value::as_str)?.to_owned(),
        }),
        _ => None,
    }
}

/// The `skill` tool body (the dsh execute, index.ts:128-158, 1:1): the
/// name-grammar gate, the cwd ensure, the list lookup, and the double
/// invocability check on both the summary and the loaded definition.
///
/// # Errors
///
/// The dsh error family verbatim (`invalid skill name`, `unknown or no
/// longer available`, `not available for model invocation`).
pub fn execute(
    runtime: &SkillRuntime,
    seams: &dyn crate::tools::Seams,
    arguments: &Value,
    agent: Option<&str>,
) -> Result<Value, BodyError> {
    let name = arguments
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            // Unreachable through the loop (the fr-tools hard validation
            // refuses a missing required argument before dispatch); the
            // value renders as its JSON form where dsh would print the
            // JS `undefined` coercion.
            let rendered = arguments
                .get("name")
                .map_or_else(|| "undefined".to_owned(), Value::to_string);
            BodyError::plain(format!("invalid skill name \"{rendered}\""))
        })?;
    if !is_skill_name(name) {
        return Err(BodyError::plain(format!("invalid skill name \"{name}\"")));
    }
    let cwd = match agent {
        Some(agent) => Some(
            seams
                .working_directory_ensure(agent)
                .map_err(BodyError::plain)?,
        ),
        None => None,
    };
    let lookup = SkillLookupOptions { cwd };
    let summary = runtime
        .registry
        .list(&lookup)
        .map_err(BodyError::plain)?
        .into_iter()
        .find(|skill| skill.name == name);
    let Some(summary) = summary else {
        return Err(BodyError::plain(format!(
            "skill \"{name}\" is unknown or no longer available"
        )));
    };
    if !is_model_invocable(&summary) {
        return Err(BodyError::plain(format!(
            "skill \"{name}\" is not available for model invocation"
        )));
    }
    let Some(skill) = runtime
        .registry
        .get(name, &lookup)
        .map_err(BodyError::plain)?
    else {
        return Err(BodyError::plain(format!(
            "skill \"{name}\" is unknown or no longer available"
        )));
    };
    if !is_model_invocable(&skill.to_summary()) {
        return Err(BodyError::plain(format!(
            "skill \"{name}\" is not available for model invocation"
        )));
    }
    let mut value = serde_json::Map::new();
    value.insert("name".to_owned(), json!(skill.name));
    value.insert("provider".to_owned(), json!(skill.provider));
    if let Some(base) = &skill.resource_base {
        value.insert("resourceBase".to_owned(), resource_base_value(base));
    }
    value.insert("content".to_owned(), json!(skill.content));
    Ok(Value::Object(value))
}

/// The `skill` render projection (the dsh `output.render`):
/// `renderSkillContent` onto one text block.
#[must_use]
pub fn render(_args: &Value, value: &Value) -> Vec<Value> {
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let resource_base = value.get("resourceBase").and_then(resource_base_from_value);
    let text = render_skill_content(name, provider, resource_base.as_ref(), content);
    vec![json!({ "type": "text", "text": text })]
}

/// The `skill` pending-call presentation (the dsh `presentCall`): one
/// generic read card naming the skill.
#[must_use]
pub fn present_call(args: &Value) -> Value {
    let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
    json!({
        "card": "generic",
        "title": format!("Load skill {name}"),
        "kind": "read",
        "rawInput": args.get("name").cloned().unwrap_or(Value::Null),
    })
}

/// Serves one `'skills'` method invocation (the single pre-step
/// selector; the unknown method is the artifact-band refusal).
///
/// # Errors
///
/// The payload decode refusal, an unknown method, or the pre-step's own
/// failure family.
pub fn serve(deps: &SkillServiceDeps<'_>, method: u32, payload: &[u8]) -> Result<Vec<u8>, String> {
    match method {
        METHOD_PRE_STEP => {
            let request: Value = if payload.is_empty() {
                json!({})
            } else {
                serde_json::from_slice(payload)
                    .map_err(|error| format!("{SERVICE_NAME} payload must be JSON: {error}"))?
            };
            let session = request
                .get("session")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{SERVICE_NAME} pre-step payload requires session"))?;
            let batch = request
                .get("messages")
                .cloned()
                .unwrap_or(Value::Array(Vec::new()));
            let messages: Vec<Message> = serde_json::from_value(batch).map_err(|error| {
                format!("{SERVICE_NAME} pre-step messages decode failed: {error}")
            })?;
            let messages = pre_step(deps, session, messages)?;
            serde_json::to_vec(&json!({ "messages": messages }))
                .map_err(|error| format!("{SERVICE_NAME} reply encode failed: {error}"))
        }
        other => Err(format!("unknown {SERVICE_NAME} method {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    /// The scripted cross-plugin seam: a fixed workdir, everything else
    /// unused (the sre `ScriptSeams` judgment).
    struct ScriptSeams {
        workdir: String,
    }

    impl crate::tools::PolicySeam for ScriptSeams {
        fn resolve_policy(&self, _session: Option<&str>) -> Result<Option<Value>, String> {
            Ok(None)
        }
    }

    impl crate::tools::ApprovalSeam for ScriptSeams {
        fn request(&self, _ask: &fr_sandbox::escalation::EscalationAsk) -> Result<String, String> {
            Err("unused".to_owned())
        }
    }

    impl crate::tools::Seams for ScriptSeams {
        fn policy(&self) -> &dyn crate::tools::PolicySeam {
            self
        }

        fn approval(&self) -> &dyn crate::tools::ApprovalSeam {
            self
        }

        fn terminals(&self) -> Option<&dyn crate::tools::TerminalSeams> {
            None
        }

        fn attachments(&self) -> Option<&dyn crate::tools::AttachmentSeam> {
            None
        }

        fn llm(&self) -> Option<&dyn crate::tools::LlmSeam> {
            None
        }

        fn route_header(&self) -> Option<&dyn crate::tools::RouteHeaderSeam> {
            None
        }

        fn shell_resolve(&self, _shell: &str, _request: &Value) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn shell_start(
            &self,
            _shell: &str,
            _spec: &Value,
            _command: &str,
            _owner: Option<&str>,
            _call_id: &str,
            _root_call_id: &str,
            _escalation_modes: &[&str],
        ) -> Result<(String, String), String> {
            Err("unused".to_owned())
        }

        fn shell_result(&self, _job_id: &str) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_wait(
            &self,
            _id: &str,
            _timeout_ms: f64,
            _caller: Option<&str>,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_list(&self, _caller: Option<&str>) -> Result<Value, String> {
            Ok(json!([]))
        }

        fn jobs_get(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("unknown job".to_owned())
        }

        fn jobs_read(&self, _id: &str, _caller: Option<&str>) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn jobs_kill(
            &self,
            _id: &str,
            _caller: Option<&str>,
            _reason: Option<&str>,
        ) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn jobs_remove(&self, _id: &str, _caller: Option<&str>) -> Result<(), String> {
            Err("unused".to_owned())
        }

        fn working_directory_ensure(&self, _agent: &str) -> Result<String, String> {
            Ok(self.workdir.clone())
        }

        fn shell_env_collect(
            &self,
            _agent: Option<&str>,
        ) -> Result<crate::shell_env::FrozenEnv, String> {
            Err("unused".to_owned())
        }

        fn subprocess_spawn(&self, _spec: &Value) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn subprocess_read(
            &self,
            _handle: &str,
            _stream: &str,
            _from: u64,
        ) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_wait(&self, _handle: &str, _timeout_ms: f64) -> Result<Value, String> {
            Err("unused".to_owned())
        }

        fn subprocess_resolve_executable(&self, _command: &str) -> Result<String, String> {
            Err("unused".to_owned())
        }

        fn spill_store_save_text(&self, _save: &Value) -> Result<Option<Value>, String> {
            Err("unused".to_owned())
        }
    }

    /// The scripted session reads: one live log per id, everything else
    /// unused (the `QueryWire` seam).
    struct FakeReads {
        log: std::sync::Mutex<Option<crate::session_query::QueryLog>>,
    }

    impl FakeReads {
        fn empty() -> Self {
            Self {
                log: std::sync::Mutex::new(None),
            }
        }
    }

    impl QueryWire for FakeReads {
        fn session_read(
            &self,
            _id: &str,
        ) -> Result<Option<crate::session_query::QueryLog>, String> {
            Ok(self.log.lock().expect("log").clone())
        }

        fn persisted_list(&self) -> Result<Vec<Value>, String> {
            Err("unused".to_owned())
        }

        fn persisted_cold_read(
            &self,
            _id: &str,
        ) -> Result<Option<crate::session_query::QueryLog>, String> {
            Err("unused".to_owned())
        }

        fn projection_state(&self, _session: &str, _key: &str) -> Result<Option<Value>, String> {
            Err("unused".to_owned())
        }
    }

    /// One deterministic identity source (the assembly's generator
    /// judgment; tests pin the emission order).
    fn counter_ids() -> std::sync::Arc<dyn Fn() -> MessageId + Send + Sync> {
        let counter = AtomicU64::new(1);
        std::sync::Arc::new(move || {
            let value = counter.fetch_add(1, AtomicOrdering::SeqCst);
            MessageId::new(format!("m-{value}"))
        })
    }

    /// One composed pre-step fixture: the runtime over runtime-registered
    /// skills, the seams, and the reads.
    struct SkillFix {
        runtime: SkillRuntime,
        seams: ScriptSeams,
        reads: FakeReads,
    }

    impl SkillFix {
        fn new() -> Self {
            let registry = Arc::new(SkillRegistry::new(128).expect("registry"));
            Self {
                runtime: SkillRuntime {
                    registry,
                    catalog_description_max_length: 500,
                    registered: AtomicBool::new(true),
                    next_id: counter_ids(),
                },
                seams: ScriptSeams {
                    workdir: "/w".to_owned(),
                },
                reads: FakeReads::empty(),
            }
        }

        /// Registers one skill spec (the disposer borrows the fixture,
        /// so every call site holds its own binding).
        fn register(&self, spec: &SkillSpec) -> Box<dyn FnOnce() + Send + '_> {
            self.runtime
                .registry
                .register(spec.definition())
                .expect("register")
        }

        fn deps(&self) -> SkillServiceDeps<'_> {
            SkillServiceDeps {
                runtime: &self.runtime,
                seams: &self.seams,
                reads: &self.reads,
            }
        }
    }

    /// One provider whose discovery never completes (the incomplete
    /// observation fixture).
    struct IncompleteProvider;

    impl crate::skill_runtime::SkillProvider for IncompleteProvider {
        fn name(&self) -> &'static str {
            "incomplete"
        }

        fn list(
            &self,
            _options: &SkillLookupOptions,
        ) -> Result<crate::skill_runtime::SkillProviderOutput, String> {
            Ok(crate::skill_runtime::SkillProviderOutput::Observation {
                candidates: Vec::new(),
                complete: false,
            })
        }

        fn get(
            &self,
            _candidate: &crate::skill_runtime::SkillCandidate,
            _options: &SkillLookupOptions,
        ) -> Result<Option<crate::skill_runtime::SkillDefinition>, String> {
            Ok(None)
        }
    }

    /// One skill spec: the registration payload plus per-test overrides.
    struct SkillSpec {
        name: &'static str,
        model_invocable: bool,
        user_invocable: bool,
    }

    impl SkillSpec {
        fn definition(&self) -> crate::skill_runtime::SkillDefinition {
            crate::skill_runtime::SkillDefinition {
                path: None,
                name: self.name.to_owned(),
                description: format!("the {} skill", self.name),
                when_to_use: None,
                invocation: crate::skill_runtime::SkillInvocationPolicy {
                    model_invocable: self.model_invocable,
                    user_invocable: self.user_invocable,
                },
                source: "custom".to_owned(),
                provider: crate::skill_runtime::RUNTIME_PROVIDER.to_owned(),
                resource_base: Some(SkillResourceBase::Directory {
                    path: format!("/r/{}", self.name),
                }),
                content: format!("body of {}", self.name),
                metadata: None,
            }
        }
    }

    impl Default for SkillSpec {
        fn default() -> Self {
            Self {
                name: "pdf",
                model_invocable: true,
                user_invocable: true,
            }
        }
    }

    /// One claimed user message (the source kind `user`, one text
    /// block).
    fn user_text(id: u64, text: &str) -> Message {
        let mut source = RawObject::new();
        source.insert("kind".to_owned(), json!("user"));
        Message::Known(Box::new(KnownMessage::User {
            id: MessageId::new(format!("claim-{id}")),
            content: vec![ContentBlock::Known(KnownContentBlock::Text {
                text: text.to_owned(),
            })],
            source: MessageSource::Other(source),
            extra: RawObject::new(),
        }))
    }

    /// One logged `user/message` event carrying a skill-catalog source
    /// with the given entries (seq marks the visibility slot).
    fn catalog_event(seq: u64, entries: &[(&str, &str)]) -> Value {
        let source = catalog_source(
            &entries
                .iter()
                .map(|(name, description)| CatalogEntry {
                    name: (*name).to_owned(),
                    description: (*description).to_owned(),
                })
                .collect::<Vec<_>>(),
            false,
        );
        json!({
            "seq": seq,
            "time": 0,
            "type": "user/message",
            "data": {
                "role": "user",
                "id": format!("catalog-{seq}"),
                "content": [{ "type": "text", "text": "catalog frame" }],
                "source": source,
            },
            "surfaceOp": "append",
        })
    }

    /// The first catalog message of one batch as a plain value view
    /// (tests read the emitted source through the same decode the
    /// history scan uses).
    fn batch_source(messages: &[Message], index: usize) -> Value {
        source_value(&messages[index]).expect("message source")
    }

    #[test]
    fn catalog_description_normalizes_and_caps_on_utf16_units() {
        assert_eq!(catalog_description("a\n  b\tc", 500), "a b c");
        assert_eq!(catalog_description("  padded  ", 500), "padded");
        // 11 units over a 10 cap: clip to 7 and append the tail.
        assert_eq!(catalog_description("abcdefghijk", 10), "abcdefg...");
        // Exactly at the cap stays whole.
        assert_eq!(catalog_description("abcdefghij", 10), "abcdefghij");
        // Each BMP ideograph is one UTF-16 unit: 7 units over a 6 cap
        // clips to 3 units before the tail.
        assert_eq!(catalog_description("一二三四五六七", 6), "一二三...");
        // An astral char is two units: 3 units stay whole, and 5 units
        // over a 4 cap clip to a single unit (empty here) plus the tail.
        assert_eq!(catalog_description("𐀀a", 4), "𐀀a");
        assert_eq!(catalog_description("𐀀abc", 4), "...");
    }

    #[test]
    fn digest_pins_the_canonical_sha256() {
        let entries = vec![CatalogEntry {
            name: "pdf".to_owned(),
            description: "Read pdf files".to_owned(),
        }];
        assert_eq!(
            digest_catalog_entries(&entries),
            "56e91de8042a0981314c444ef5712ee10ac1a61191271a192b90b4c9f150323f"
        );
        assert_eq!(
            digest_catalog_entries(&[]),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn read_catalog_entries_rejects_unreadable_records() {
        let ok = json!({
            "kind": "skill-catalog",
            "form": "catalog",
            "entries": [{ "name": "pdf", "description": "d" }],
        });
        let entries = read_catalog_entries(&ok).expect("readable");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "pdf");
        // Producer-added fields ride along; the entries decode anyway.
        let with_extra = json!({
            "kind": "skill-catalog",
            "form": "catalog",
            "entries": [{ "name": "pdf", "description": "d", "seq": 7 }],
        });
        assert!(read_catalog_entries(&with_extra).is_some());
        // The refusal family: absent or malformed records.
        assert!(read_catalog_entries(&json!({ "kind": "skill-catalog" })).is_none());
        assert!(read_catalog_entries(&json!({ "entries": "no" })).is_none());
        assert!(read_catalog_entries(&json!({ "entries": ["no"] })).is_none());
        assert!(read_catalog_entries(&json!({ "entries": [{ "description": "d" }] })).is_none());
        assert!(
            read_catalog_entries(&json!({ "entries": [{ "name": "", "description": "d" }] }))
                .is_none()
        );
        assert!(
            read_catalog_entries(&json!({ "entries": [{ "name": "pdf", "description": 7 }] }))
                .is_none()
        );
    }

    #[test]
    fn resolve_rejects_caps_below_three_verbatim() {
        assert_eq!(
            resolve_catalog_description_max_length(2).expect_err("refused"),
            "tool-skill: catalogDescriptionMaxLength must be an integer greater than or equal to 3"
        );
        assert_eq!(resolve_catalog_description_max_length(3).expect("floor"), 3);
    }

    #[test]
    fn catalog_frames_pin_the_verbatim_text_and_sources() {
        let ids = counter_ids();
        let entries = vec![CatalogEntry {
            name: "pdf".to_owned(),
            description: "a<b & \"quoted\"".to_owned(),
        }];
        let initial = render_catalog_message(&*ids, &entries);
        let source = batch_source(std::slice::from_ref(&initial), 0);
        assert_eq!(source.get("kind"), Some(&json!("skill-catalog")));
        assert_eq!(source.get("form"), Some(&json!("catalog")));
        assert!(
            source.get("update").is_none(),
            "no update key on the first frame"
        );
        // The source stores the raw description; the escaping belongs to
        // the rendered line only.
        assert_eq!(
            source.get("entries"),
            Some(&json!([{ "name": "pdf", "description": "a<b & \"quoted\"" }]))
        );
        let Message::Known(boxed) = &initial else {
            panic!("known message");
        };
        let KnownMessage::User { id, content, .. } = boxed.as_ref() else {
            panic!("user role");
        };
        assert_eq!(id.as_str(), "m-1");
        let ContentBlock::Known(KnownContentBlock::Text { text }) = &content[0] else {
            panic!("text block");
        };
        assert_eq!(
            text,
            "<system-reminder>\nA skill is a reusable set of task-specific instructions. The following skills are available in this session:\n\n<available_skills>\n- `pdf`: a&lt;b &amp; \"quoted\"\n</available_skills>\n\nIf the user names a skill, or the task clearly matches a skill's description, call the `skill` tool with the exact skill name before taking task actions. Load all applicable skills, then follow their full instructions. This catalog contains summaries only; do not infer or follow a skill's instructions until it has been loaded.\nA user may also invoke a skill directly; its <skill_content> block then appears in this conversation. Follow it, and do not call the `skill` tool again for that skill.\n</system-reminder>"
        );
        // The replacement frame swaps the prose and rides update: true.
        let update = render_catalog_update(&*ids, &entries);
        let source = batch_source(std::slice::from_ref(&update), 0);
        assert_eq!(source.get("update"), Some(&json!(true)));
        let Message::Known(boxed) = &update else {
            panic!("known message");
        };
        let KnownMessage::User { content, .. } = boxed.as_ref() else {
            panic!("user role");
        };
        let ContentBlock::Known(KnownContentBlock::Text { text }) = &content[0] else {
            panic!("text block");
        };
        assert!(text.contains("The available skill catalog changed. This complete catalog replaces every earlier available-skills list in this session:"));
        assert!(text.contains("Use only names in this replacement catalog."));
        // The empty replacement carries the unavailable prose.
        let empty = render_catalog_update(&*ids, &[]);
        let Message::Known(boxed) = &empty else {
            panic!("known message");
        };
        let KnownMessage::User { content, .. } = boxed.as_ref() else {
            panic!("user role");
        };
        let ContentBlock::Known(KnownContentBlock::Text { text }) = &content[0] else {
            panic!("text block");
        };
        assert!(text.contains("No skills are currently available through the `skill` tool. Do not use names from earlier skill catalogs."));
    }

    #[test]
    fn catalog_pass_publishes_then_reuses_the_visible_history() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec {
            name: "pdf",
            ..SkillSpec::default()
        });
        let deps = fix.deps();
        // First step: the claimed batch grows by the initial frame.
        let first = pre_step(&deps, "s1", vec![user_text(1, "hi")]).expect("pre-step");
        assert_eq!(first.len(), 2, "claimed + catalog");
        assert_eq!(source_kind(&first[1]).as_deref(), Some("skill-catalog"));
        // The frame joins the log as a visible user/message event.
        let emitted = match &first[1] {
            Message::Known(boxed) => serde_json::to_value(boxed.as_ref()).expect("serialize"),
            Message::Other(_) => panic!("known"),
        };
        fix.reads
            .log
            .lock()
            .expect("log")
            .replace(crate::session_query::QueryLog {
                header: json!({ "id": "s1" }),
                inherited_event_count: 0,
                events: vec![json!({
                    "seq": 0,
                    "time": 0,
                    "type": "user/message",
                    "data": emitted,
                    "surfaceOp": "append",
                })],
            });
        // Second step: the visible history digest matches, so the batch
        // stays untouched.
        let second = pre_step(&deps, "s1", vec![user_text(2, "again")]).expect("pre-step");
        assert_eq!(second.len(), 1, "no republish");
    }

    #[test]
    fn catalog_pass_republishes_a_replacement_when_skills_change() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        let deps = fix.deps();
        // History: one visible catalog with a different entry list.
        fix.reads
            .log
            .lock()
            .expect("log")
            .replace(crate::session_query::QueryLog {
                header: json!({ "id": "s1" }),
                inherited_event_count: 0,
                events: vec![catalog_event(0, &[("old", "the old skill")])],
            });
        let out = pre_step(&deps, "s1", vec![user_text(1, "hi")]).expect("pre-step");
        assert_eq!(out.len(), 2);
        let source = batch_source(&out, 1);
        assert_eq!(
            source.get("update"),
            Some(&json!(true)),
            "replacement frame"
        );
        assert_eq!(
            source.get("entries"),
            Some(&json!([{ "name": "pdf", "description": "the pdf skill" }]))
        );
    }

    #[test]
    fn catalog_pass_removes_unpublished_empty_frames_and_waits_for_completeness() {
        // No skills, no history, and a stray catalog message in the
        // batch: the unpublished-empty arm removes it.
        let fix = SkillFix::new();
        let deps = fix.deps();
        let ids = counter_ids();
        let stray = render_catalog_message(
            &*ids,
            &[CatalogEntry {
                name: "stale".to_owned(),
                description: "stale".to_owned(),
            }],
        );
        let out = pre_step(&deps, "s1", vec![user_text(1, "hi"), stray]).expect("pre-step");
        assert_eq!(out.len(), 1, "the stray frame is removed");
        // And an empty batch stays empty.
        let out = pre_step(&deps, "s1", vec![user_text(2, "again")]).expect("pre-step");
        assert_eq!(out.len(), 1, "nothing is published without skills");
        // An incomplete observation leaves the batch untouched.
        let fix = SkillFix::new();
        let _disposer = fix
            .runtime
            .registry
            .register_provider(|_control| {
                Ok(Box::new(IncompleteProvider) as Box<dyn crate::skill_runtime::SkillProvider>)
            })
            .expect("register");
        let deps = fix.deps();
        let out = pre_step(&deps, "s1", vec![user_text(3, "hi")]).expect("pre-step");
        assert_eq!(out.len(), 1, "incomplete snapshots never publish");
    }

    #[test]
    fn catalog_pass_freezes_when_the_tool_is_not_visible() {
        // The invisible arm snapshots empty and complete: with no
        // history the batch is untouched even though skills exist.
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        fix.runtime.registered.store(false, AtomicOrdering::SeqCst);
        let deps = fix.deps();
        let out = pre_step(&deps, "s1", vec![user_text(1, "hi")]).expect("pre-step");
        assert_eq!(out.len(), 1, "no catalog without tool visibility");
    }

    #[test]
    fn gesture_pass_injects_user_invocable_skills_once() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        let deps = fix.deps();
        let out = pre_step(
            &deps,
            "s1",
            vec![user_text(1, "please /pdf the report, then /pdf again")],
        )
        .expect("pre-step");
        assert_eq!(out.len(), 3, "claimed + catalog + one injection");
        let source = batch_source(&out, 2);
        assert_eq!(source.get("kind"), Some(&json!("skill-invocation")));
        assert_eq!(source.get("name"), Some(&json!("pdf")));
        assert_eq!(source.get("form"), Some(&json!("instructions")));
        let Message::Known(boxed) = &out[2] else {
            panic!("known message");
        };
        let KnownMessage::User { id, content, .. } = boxed.as_ref() else {
            panic!("user role");
        };
        assert_eq!(id.as_str(), "m-2", "the catalog frame took m-1");
        let ContentBlock::Known(KnownContentBlock::Text { text }) = &content[0] else {
            panic!("text block");
        };
        assert_eq!(
            text.as_str(),
            render_skill_content(
                "pdf",
                "runtime",
                Some(&SkillResourceBase::Directory {
                    path: "/r/pdf".to_owned(),
                }),
                "body of pdf",
            )
        );
    }

    #[test]
    fn gesture_pass_keeps_prose_and_foreign_sources_plain() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        let _quiet = fix.register(&SkillSpec {
            name: "quiet",
            model_invocable: true,
            user_invocable: false,
        });
        // History guarantees the catalog pass publishes (so injection
        // counts stay readable).
        let deps = fix.deps();
        // Paths, fractions, periods, unknown names, and non-user
        // sources never gesture.
        let out = pre_step(
            &deps,
            "s1",
            vec![user_text(1, "see /usr/bin, score 5/8, run /pdf.")],
        )
        .expect("pre-step");
        assert_eq!(out.len(), 2, "prose only: claimed + catalog");
        // A user-invocable hit at the exact end of the text gestures.
        let out = pre_step(&deps, "s1", vec![user_text(2, "run /pdf")]).expect("pre-step");
        assert_eq!(out.len(), 3, "end-of-text gesture");
        // The quiet skill is model-visible but never user-invocable.
        let out = pre_step(&deps, "s1", vec![user_text(3, "run /quiet")]).expect("pre-step");
        assert_eq!(out.len(), 2, "user-disabled skills stay prose");
        // Unknown names stay prose.
        let out = pre_step(&deps, "s1", vec![user_text(4, "run /nope")]).expect("pre-step");
        assert_eq!(out.len(), 2, "unknown names stay prose");
        // The catalog frame itself carries no gesture authority.
        let claimed = vec![user_text(5, "run /pdf"), out_first_catalog(&deps)];
        let out = pre_step(&deps, "s1", claimed).expect("pre-step");
        assert_eq!(
            out.len(),
            2 + usize::from(true),
            "one injection from the claimed message only"
        );
    }

    /// Builds one durable catalog message whose body names `/pdf` for
    /// the foreign-source probe (the frame's text gestures for nobody).
    fn out_first_catalog(deps: &SkillServiceDeps<'_>) -> Message {
        let entries = vec![CatalogEntry {
            name: "pdf".to_owned(),
            description: "use /pdf to load it".to_owned(),
        }];
        render_catalog_message(&*deps.runtime.next_id, &entries)
    }

    #[test]
    fn tool_execute_pins_the_error_family_and_the_value_shape() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        let seams = &fix.seams;
        let runtime = &fix.runtime;
        // The grammar gate.
        let error = execute(runtime, seams, &json!({ "name": "PDF" }), None)
            .expect_err("refused")
            .message;
        assert_eq!(error, "invalid skill name \"PDF\"");
        // Unknown name after the registry lookup.
        let error = execute(runtime, seams, &json!({ "name": "zip" }), None)
            .expect_err("refused")
            .message;
        assert_eq!(error, "skill \"zip\" is unknown or no longer available");
        // A model-disabled skill is invisible to the loader.
        let quiet = fix
            .runtime
            .registry
            .register(crate::skill_runtime::SkillDefinition {
                invocation: crate::skill_runtime::SkillInvocationPolicy {
                    model_invocable: false,
                    user_invocable: true,
                },
                ..SkillSpec {
                    name: "quiet",
                    model_invocable: true,
                    user_invocable: true,
                }
                .definition()
            })
            .expect("register");
        let _quiet = quiet;
        let error = execute(runtime, seams, &json!({ "name": "quiet" }), None)
            .expect_err("refused")
            .message;
        assert_eq!(
            error,
            "skill \"quiet\" is not available for model invocation"
        );
        // The happy path rides the cwd ensure and the double lookup.
        let value = execute(runtime, seams, &json!({ "name": "pdf" }), Some("s1")).expect("loaded");
        assert_eq!(
            value,
            json!({
                "name": "pdf",
                "provider": "runtime",
                "resourceBase": { "kind": "directory", "path": "/r/pdf" },
                "content": "body of pdf",
            })
        );
    }

    #[test]
    fn tool_render_and_present_call_pin_the_faces() {
        let value = json!({
            "name": "pdf",
            "provider": "filesystem",
            "resourceBase": { "kind": "directory", "path": "/r/pdf" },
            "content": "body",
        });
        let rendered = render(&json!({}), &value);
        assert_eq!(
            rendered,
            vec![json!({
                "type": "text",
                "text": render_skill_content(
                    "pdf",
                    "filesystem",
                    Some(&SkillResourceBase::Directory {
                        path: "/r/pdf".to_owned(),
                    }),
                    "body",
                ),
            })]
        );
        let view = present_call(&json!({ "name": "pdf" }));
        assert_eq!(
            view,
            json!({
                "card": "generic",
                "title": "Load skill pdf",
                "kind": "read",
                "rawInput": "pdf",
            })
        );
    }

    #[test]
    fn resource_bases_round_trip_over_the_wire_shapes() {
        for base in [
            SkillResourceBase::Directory {
                path: "/r".to_owned(),
            },
            SkillResourceBase::Url {
                url: "https://x".to_owned(),
            },
            SkillResourceBase::Opaque {
                description: "ask".to_owned(),
            },
        ] {
            let encoded = resource_base_value(&base);
            assert_eq!(resource_base_from_value(&encoded), Some(base));
        }
        assert_eq!(resource_base_from_value(&json!({ "kind": "bogus" })), None);
    }

    #[test]
    fn serve_round_trips_the_pre_step_and_refuses_unknown_methods() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        let deps = fix.deps();
        let payload = serde_json::to_vec(&json!({
            "session": "s1",
            "messages": [serde_json::to_value(user_text(1, "hi")).expect("encode")],
        }))
        .expect("encode");
        let reply = serve(&deps, METHOD_PRE_STEP, &payload).expect("served");
        let reply: Value = serde_json::from_slice(&reply).expect("reply");
        let messages = reply
            .get("messages")
            .and_then(Value::as_array)
            .expect("batch");
        assert_eq!(messages.len(), 2, "claimed + catalog");
        // A missing session is the payload refusal.
        let payload = serde_json::to_vec(&json!({ "messages": [] })).expect("encode");
        let error = serve(&deps, METHOD_PRE_STEP, &payload).expect_err("refused");
        assert!(error.contains("requires session"), "{error}");
        // Unknown selectors name the band.
        let error = serve(&deps, 42, b"{}").expect_err("refused");
        assert_eq!(error, "unknown skills method 42");
    }

    #[test]
    fn register_liveness_is_the_visibility_carrier() {
        let fix = SkillFix::new();
        let _pdf = fix.register(&SkillSpec::default());
        assert!(fix.runtime.tool_visible());
        fix.runtime.mark_registered();
        assert!(fix.runtime.tool_visible());
        fix.runtime.registered.store(false, AtomicOrdering::SeqCst);
        assert!(!fix.runtime.tool_visible());
    }
}
