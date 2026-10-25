//! Content-block projection for request assembly, verbatim to dsh
//! `dsh-llm/content.ts`.
//!
//! Files project to deterministic handle text for every route, images
//! project to route-owned substitute text, the offload requirement is
//! computed fail-closed without offloading, and tool updates project per
//! route capability.
//!
//! The substitute texts are protocol-facing and therefore verbatim; the
//! offloaded-image set is a durable surface fact shared by every route.
//!
//! Messages of an unknown role (the forward-compat `Message::Other` face)
//! pass through untouched: dsh's message-role map is closed, so there is no
//! dsh behavior to mirror inside a role it cannot represent.

use crate::adapter::RequestMessage;
use fr_wire::call_config::{ToolHistory, ToolSchema, ToolUpdate};
use fr_wire::content::{FileAttachmentRef, ImageAttachmentRef, ImageMediaType};
use fr_wire::{ContentBlock, KnownContentBlock, TrueOnly};
use std::collections::HashSet;

fn quoted(value: &str) -> String {
    // `&str` serialization cannot fail; the justified expect follows the
    // crate precedent for truly infallible conversions (error.rs regexes).
    serde_json::to_string(value).expect("&str serialization cannot fail")
}

/// The first eight hex characters after the `sha256:` prefix (dsh
/// `String(id).slice('sha256:'.length, 'sha256:'.length + 8)`).
fn digest_prefix(attachment_id: &str) -> String {
    attachment_id.chars().skip(7).take(8).collect()
}

fn image_identity(reference: &ImageAttachmentRef) -> String {
    reference.name.as_ref().map_or_else(
        || reference.attachment_id.as_str().to_owned(),
        |name| format!("{} ({})", quoted(name), reference.attachment_id),
    )
}

const fn extension(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => ".png",
        ImageMediaType::Jpeg => ".jpg",
        ImageMediaType::Webp => ".webp",
        ImageMediaType::Gif => ".gif",
    }
}

const fn media_type_value(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "image/png",
        ImageMediaType::Jpeg => "image/jpeg",
        ImageMediaType::Webp => "image/webp",
        ImageMediaType::Gif => "image/gif",
    }
}

/// Execution-world path that model tools can use to read one normalized
/// image (dsh `ImageAttachmentAccess`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageAttachmentAccess {
    /// Absolute path to immutable normalized bytes; read-only for callers.
    pub readonly_path: String,
}

fn normalized_access_text(
    reference: &ImageAttachmentRef,
    access: &ImageAttachmentAccess,
) -> String {
    format!(
        " Normalized copy (read-only; may be resized or re-encoded): {} ({}x{}px, {}). Source dimensions, format, and byte size may differ. Copy to a writable path ending in {} before editing.",
        quoted(&access.readonly_path),
        reference.width,
        reference.height,
        media_type_value(reference.media_type),
        extension(reference.media_type),
    )
}

/// Stable text shown to a model that cannot accept one durable image
/// reference (dsh `textOnlyImageText`).
#[must_use]
pub fn text_only_image_text(reference: &ImageAttachmentRef) -> String {
    let digest = digest_prefix(reference.attachment_id.as_str());
    format!("[image omitted because this model accepts text only; attachment sha256:{digest}]")
}

/// Stable model-facing handle for one exact request image (dsh
/// `requestImageHandleText`): identity, request preview dimensions, and the
/// normalized-copy access note when one resolves.
#[must_use]
pub fn request_image_handle_text(
    reference: &ImageAttachmentRef,
    version: (u32, u32),
    access: Option<&ImageAttachmentAccess>,
) -> String {
    let preview = format!(
        "Image {}; request preview {}x{}px.",
        image_identity(reference),
        version.0,
        version.1
    );
    match access {
        None => format!(
            "{preview} It may be resized or re-encoded; source dimensions, format, and byte size may differ."
        ),
        Some(access) => preview + &normalized_access_text(reference, access),
    }
}

/// Stable per-image substitute for a request-limit omission (dsh
/// `offloadedImageText`).
#[must_use]
pub fn offloaded_image_text(
    reference: &ImageAttachmentRef,
    access: Option<&ImageAttachmentAccess>,
) -> String {
    let identity = format!(
        "image omitted to fit request image limits; {}.",
        image_identity(reference)
    );
    access.map_or_else(
        || {
            format!("[{identity} No local normalized image path is available; ask the user to attach it again if needed.]")
        },
        |access| format!("[{identity}{}]", normalized_access_text(reference, access)),
    )
}

/// Stable model-facing handle for one durable file reference (dsh
/// `fileHandleText`).
///
/// The text names the address of the verbatim stored copy and the
/// instruction to read it on demand; this is the only representation a
/// provider ever receives for a file.
#[must_use]
pub fn file_handle_text(reference: &FileAttachmentRef, readonly_path: Option<&str>) -> String {
    let digest = digest_prefix(reference.attachment_id.as_str());
    let identity = format!(
        "File {} ({} bytes, sha256:{digest})",
        quoted(&reference.name),
        reference.bytes
    );
    readonly_path.map_or_else(
        || {
            format!("[{identity} was uploaded, but the current execution environment cannot access a readable path. Report that limitation if its contents are needed; do not claim to have read it.]")
        },
        |path| {
            format!(
                "[{identity}: verbatim read-only copy saved at {}. Read that path with your file tools when its contents are needed; copy it to a writable location before modifying it. When delegating file work, include this saved path in the delegation prompt; only subagents sharing this execution environment can read it.]",
                quoted(path)
            )
        },
    )
}

/// True when typed model content contains an image block (dsh
/// `contentHasImage`): the one image walk shared by every image policy.
#[must_use]
pub fn content_has_image(content: &[ContentBlock]) -> bool {
    content
        .iter()
        .any(|block| matches!(block, ContentBlock::Known(KnownContentBlock::Image { .. })))
}

/// True when typed model content contains a file block (dsh
/// `contentHasFile`).
#[must_use]
pub fn content_has_file(content: &[ContentBlock]) -> bool {
    content
        .iter()
        .any(|block| matches!(block, ContentBlock::Known(KnownContentBlock::File { .. })))
}

/// Apply a block-level replacement to one message; `None` keeps the
/// original message value untouched.
fn replace_blocks(
    message: &RequestMessage,
    replace: impl Fn(&[ContentBlock]) -> Option<Vec<ContentBlock>>,
) -> RequestMessage {
    let blocks = message.content_blocks();
    replace(&blocks).map_or_else(|| message.clone(), |next| message.with_content_blocks(next))
}

/// Replace every file occurrence with handle text; `None` when unchanged.
fn replace_files_with_handles(
    blocks: &[ContentBlock],
    resolve_path: &dyn Fn(&FileAttachmentRef) -> Option<String>,
) -> Option<Vec<ContentBlock>> {
    let mut next: Option<Vec<ContentBlock>> = None;
    for (index, block) in blocks.iter().enumerate() {
        if let ContentBlock::Known(KnownContentBlock::File { attachment }) = block {
            let buffer = next.get_or_insert_with(|| blocks[..index].to_vec());
            let text = file_handle_text(attachment, resolve_path(attachment).as_deref());
            buffer.push(ContentBlock::Known(KnownContentBlock::Text { text }));
            continue;
        }
        if let Some(buffer) = next.as_mut() {
            buffer.push(block.clone());
        }
    }
    next
}

/// Project request file content into deterministic handle text for every
/// model route (dsh `projectFilesToText`).
///
/// No provider receives file blocks natively, so this projection is
/// unconditional in request assembly.
#[must_use]
pub fn project_files_to_text(
    messages: &[RequestMessage],
    resolve_path: &dyn Fn(&FileAttachmentRef) -> Option<String>,
) -> Vec<RequestMessage> {
    let has_file = messages
        .iter()
        .any(|message| content_has_file(&message.content_blocks()));
    if !has_file {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|message| {
            replace_blocks(message, |blocks| {
                replace_files_with_handles(blocks, resolve_path)
            })
        })
        .collect()
}

/// Replace every offloaded image occurrence with its substitute; `None`
/// when unchanged.
fn replace_offloaded_images(
    blocks: &[ContentBlock],
    substitute: &dyn Fn(&ImageAttachmentRef) -> String,
) -> Option<Vec<ContentBlock>> {
    let mut next: Option<Vec<ContentBlock>> = None;
    for (index, block) in blocks.iter().enumerate() {
        let ContentBlock::Known(KnownContentBlock::Image {
            attachment,
            offloaded,
        }) = block
        else {
            if let Some(buffer) = next.as_mut() {
                buffer.push(block.clone());
            }
            continue;
        };
        if offloaded.is_none() {
            if let Some(buffer) = next.as_mut() {
                buffer.push(block.clone());
            }
            continue;
        }
        let buffer = next.get_or_insert_with(|| blocks[..index].to_vec());
        let text = substitute(attachment);
        buffer.push(ContentBlock::Known(KnownContentBlock::Text { text }));
    }
    next
}

/// Project the surface's offloaded occurrences into deterministic text for
/// one request (dsh `projectOffloadedImages`): the offloaded set is a
/// durable surface fact, so every route sends the same set.
#[must_use]
pub fn project_offloaded_images(
    messages: &[RequestMessage],
    substitute: &dyn Fn(&ImageAttachmentRef) -> String,
) -> Vec<RequestMessage> {
    messages
        .iter()
        .map(|message| {
            replace_blocks(message, |blocks| {
                replace_offloaded_images(blocks, substitute)
            })
        })
        .collect()
}

/// Which byte accounting a route's image budget applies (dsh
/// `LlmImageRequestBudget.representation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageRepresentation {
    /// Raw file bytes.
    #[default]
    Raw,
    /// Inline base64 length, including padding.
    Base64,
}

/// Request-image budget one exact image-capable route enforces (dsh
/// `LlmImageRequestBudget`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageRequestBudget {
    /// Whether the route accounts raw file bytes or inline base64 length.
    pub representation: ImageRepresentation,
    /// Accumulated represented image bytes the route accepts.
    pub max_bytes: Option<u64>,
    /// Image occurrences the route accepts.
    pub max_images: Option<u64>,
    /// Represented bytes removed as one deterministic advance step.
    pub byte_quantum: Option<u64>,
    /// Occurrences removed as one deterministic advance step.
    pub count_quantum: Option<u64>,
}

/// Base64 length of raw image bytes, including padding.
const fn base64_length(bytes: u64) -> u64 {
    match bytes.checked_add(2) {
        Some(padded) => (padded / 3).saturating_mul(4),
        None => u64::MAX,
    }
}

/// Number of oldest retained image occurrences one route budget removes,
/// in whole count and byte quanta, once the budget is exceeded (dsh
/// `offloadedImagePrefixCount`).
///
/// A quantum of zero is a caller contract violation upstream of route
/// configuration validation; dsh's degenerate `NaN` comparison there never
/// breaks the scan, which this port mirrors with an unreachable
/// `u64::MAX` target.
fn offloaded_image_prefix_count(lengths: &[u64], budget: &ImageRequestBudget) -> u64 {
    let total: u64 = lengths
        .iter()
        .fold(0u64, |sum, bytes| sum.saturating_add(*bytes));
    let excess_count = budget
        .max_images
        .map_or(0, |limit| (lengths.len() as u64).saturating_sub(limit));
    let excess_bytes = budget
        .max_bytes
        .map_or(0, |limit| total.saturating_sub(limit));
    if excess_count == 0 && excess_bytes == 0 {
        return 0;
    }
    // Zero quanta mirror dsh's NaN degenerate: the target is never reached,
    // so the scan removes every occurrence.
    let count_quantum = budget.count_quantum.unwrap_or(1);
    let byte_quantum = budget.byte_quantum.unwrap_or(1);
    let remove_count = if excess_count == 0 {
        0
    } else if count_quantum == 0 {
        u64::MAX
    } else {
        excess_count.div_ceil(count_quantum) * count_quantum
    };
    let remove_bytes_cap = if excess_bytes == 0 {
        0
    } else if byte_quantum == 0 {
        u64::MAX
    } else {
        excess_bytes.div_ceil(byte_quantum) * byte_quantum
    };
    let mut count = 0;
    let mut removed_bytes = 0;
    for image_bytes in lengths {
        // dsh's quantum-boundary asymmetry is pinned: equality satisfies the
        // byte target only at `byteQuantum === 1`.
        let byte_target_met = remove_bytes_cap == 0
            || if byte_quantum == 1 {
                removed_bytes >= remove_bytes_cap
            } else {
                removed_bytes > remove_bytes_cap
            };
        if count >= remove_count && byte_target_met {
            break;
        }
        removed_bytes = removed_bytes.saturating_add(*image_bytes);
        count += 1;
    }
    count
}

/// Number of oldest retained occurrences a route must still offload before
/// a derived request fits its budget (dsh `requiredImageOffload`).
///
/// The route fails with `IMAGE_OFFLOAD_REQUIRED` carrying this count
/// instead of offloading on its own.
#[must_use]
pub fn required_image_offload(
    messages: &[RequestMessage],
    budget: &ImageRequestBudget,
    version_bytes: impl Fn(&ImageAttachmentRef) -> u64,
) -> u64 {
    let mut lengths: Vec<u64> = Vec::new();
    for message in messages {
        for block in message.content_blocks() {
            let ContentBlock::Known(KnownContentBlock::Image {
                attachment,
                offloaded,
            }) = &block
            else {
                continue;
            };
            if offloaded.is_some() {
                continue;
            }
            let bytes = version_bytes(attachment);
            lengths.push(match budget.representation {
                ImageRepresentation::Raw => bytes,
                ImageRepresentation::Base64 => base64_length(bytes),
            });
        }
    }
    offloaded_image_prefix_count(&lengths, budget)
}

/// Replace every image occurrence for a text-only model; `None` when
/// unchanged.
fn replace_images_for_text_model(blocks: &[ContentBlock]) -> Option<Vec<ContentBlock>> {
    let mut next: Option<Vec<ContentBlock>> = None;
    for (index, block) in blocks.iter().enumerate() {
        let ContentBlock::Known(KnownContentBlock::Image { attachment, .. }) = block else {
            if let Some(buffer) = next.as_mut() {
                buffer.push(block.clone());
            }
            continue;
        };
        let buffer = next.get_or_insert_with(|| blocks[..index].to_vec());
        let text = text_only_image_text(attachment);
        buffer.push(ContentBlock::Known(KnownContentBlock::Text { text }));
    }
    next
}

/// Project request image content into deterministic text for an exact
/// text-only model (dsh `projectImagesForTextModel`).
#[must_use]
pub fn project_images_for_text_model(messages: &[RequestMessage]) -> Vec<RequestMessage> {
    let has_image = messages
        .iter()
        .any(|message| content_has_image(&message.content_blocks()));
    if !has_image {
        return messages.to_vec();
    }
    messages
        .iter()
        .map(|message| replace_blocks(message, replace_images_for_text_model))
        .collect()
}

/// Request messages and tools after one route's tool update projection
/// (dsh `ProjectedToolUpdates`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedToolUpdates {
    /// History with only the developer updates this route supports.
    pub messages: Vec<RequestMessage>,
    /// Provider declarations, including deferred and retained definitions
    /// when the route supports them.
    pub tools: Option<Vec<ToolSchema>>,
}

fn without_developer_messages(messages: &[RequestMessage]) -> Vec<RequestMessage> {
    messages
        .iter()
        .filter(|message| message.role_name() != Some("developer"))
        .cloned()
        .collect()
}

/// Build the declaration map from session-folded history (dsh
/// `toolDeclarations`): later additions activate undeclared definitions at
/// their recorded position; `in-history` keeps removed definitions,
/// `addition-only` omits inactive ones.
fn tool_declarations(
    tools: Option<&[ToolSchema]>,
    mode: ToolUpdate,
    history: &ToolHistory,
) -> Vec<(String, ToolSchema)> {
    // Insertion order carries dsh Map semantics: first occurrence position
    // retained, later activation unchanged.
    let mut declarations: Vec<(String, ToolSchema)> = history
        .tools
        .iter()
        .map(|tool| (tool.name.clone(), tool.clone()))
        .collect();
    for update in &history.updates {
        for tool in &update.additions {
            if !declarations.iter().any(|(name, _)| name == &tool.name) {
                let mut activated = tool.clone();
                activated.defer_loading = Some(TrueOnly);
                declarations.push((tool.name.clone(), activated));
            }
        }
    }
    if mode == ToolUpdate::AdditionOnly {
        // Without removal support, the declaration list must omit inactive
        // tools.
        let active: HashSet<&str> = tools
            .into_iter()
            .flatten()
            .map(|tool| tool.name.as_str())
            .collect();
        declarations.retain(|(name, _)| active.contains(name.as_str()));
    }
    declarations
}

/// Construct provider declarations from session-folded history without
/// changing logged active tools (dsh `projectToolUpdates`).
///
/// Unsupported routes and incomplete history use current declarations
/// without developer updates. Explicitly deferred baseline tools become
/// available only after their first retained addition.
#[must_use]
pub fn project_tool_updates(
    messages: &[RequestMessage],
    tools: Option<&[ToolSchema]>,
    tool_update: Option<ToolUpdate>,
    history: Option<&ToolHistory>,
) -> ProjectedToolUpdates {
    let Some(tool_update) = tool_update else {
        // Unsupported routes need immediately available tools and no update
        // messages.
        let immediate = tools.map(|schemas| {
            if schemas
                .iter()
                .any(|tool| tool.defer_loading == Some(TrueOnly))
            {
                schemas
                    .iter()
                    .map(|tool| {
                        let mut immediate_tool = tool.clone();
                        immediate_tool.defer_loading = None;
                        immediate_tool
                    })
                    .collect()
            } else {
                schemas.to_vec()
            }
        });
        return ProjectedToolUpdates {
            messages: without_developer_messages(messages),
            tools: immediate,
        };
    };
    let Some(history) = history else {
        // Current schemas alone cannot resolve definitions referenced by
        // past updates.
        return ProjectedToolUpdates {
            messages: without_developer_messages(messages),
            tools: tools.map(<[ToolSchema]>::to_vec),
        };
    };
    let message_ids: HashSet<String> = messages
        .iter()
        .filter(|message| message.role_name() == Some("developer"))
        .filter_map(RequestMessage::id)
        .map(|id| id.to_string())
        .collect();
    if history
        .updates
        .iter()
        .any(|update| !message_ids.contains(update.message_id.as_str()))
    {
        // An auxiliary prefix may omit updates needed to activate
        // historical declarations.
        return ProjectedToolUpdates {
            messages: without_developer_messages(messages),
            tools: tools.map(<[ToolSchema]>::to_vec),
        };
    }
    let declarations = tool_declarations(tools, tool_update, history);
    let update_ids: HashSet<String> = history
        .updates
        .iter()
        .map(|update| update.message_id.to_string())
        .collect();
    // Deferred baseline tools still need their first addition to become
    // available.
    let mut offered: HashSet<String> = history
        .tools
        .iter()
        .filter(|tool| tool.defer_loading.is_none())
        .map(|tool| tool.name.clone())
        .collect();
    let mut projected: Vec<RequestMessage> = Vec::new();
    for message in messages {
        if message.role_name() != Some("developer") {
            projected.push(message.clone());
            continue;
        }
        // Earlier declaration series do not govern the current tool set.
        let Some(id) = message.id() else { continue };
        if !update_ids.contains(&id.to_string()) {
            continue;
        }
        let original = message.content_blocks();
        let mut content: Vec<ContentBlock> = Vec::new();
        for block in original {
            match &block {
                ContentBlock::Known(KnownContentBlock::ToolAddition { tool_name }) => {
                    // Only declared tools that are not already available
                    // need activation.
                    let declared = declarations.iter().any(|(name, _)| name == tool_name);
                    if !declared || offered.contains(tool_name) {
                        continue;
                    }
                    offered.insert(tool_name.clone());
                    content.push(block);
                }
                ContentBlock::Known(KnownContentBlock::ToolRemoval { tool_name }) => {
                    // Addition-only routes cannot deactivate a tool through
                    // history.
                    if tool_update != ToolUpdate::InHistory {
                        continue;
                    }
                    if !offered.remove(tool_name) {
                        continue;
                    }
                    content.push(block);
                }
                // Other core and plugin-defined blocks retain their content
                // and order.
                _ => content.push(block),
            }
        }
        if content.is_empty() {
            continue;
        }
        projected.push(message.with_content_blocks(content));
    }
    ProjectedToolUpdates {
        messages: projected,
        tools: Some(declarations.into_iter().map(|(_, schema)| schema).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::ids::{AttachmentId, MessageId};
    use fr_wire::message::KnownMessage;
    use fr_wire::{KnownMessageSource, Message, MessageSource, RawObject};

    fn image_ref(name: Option<&str>) -> ImageAttachmentRef {
        ImageAttachmentRef {
            attachment_id: AttachmentId::new("sha256:abcd1234ef567890"),
            media_type: ImageMediaType::Png,
            bytes: 100,
            width: 64,
            height: 64,
            name: name.map(str::to_owned),
            original_dimensions: None,
        }
    }

    fn file_ref() -> FileAttachmentRef {
        FileAttachmentRef {
            attachment_id: AttachmentId::new("sha256:abcd1234ef567890"),
            name: "report.txt".to_owned(),
            bytes: 11,
        }
    }

    fn user_message(blocks: Vec<ContentBlock>) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(KnownMessage::User {
            id: MessageId::new("m-1"),
            content: blocks,
            source: MessageSource::Known(KnownMessageSource::User {
                extra: RawObject::new(),
            }),
            extra: RawObject::new(),
        })))
    }

    fn developer_message(id: &str, blocks: Vec<ContentBlock>) -> RequestMessage {
        RequestMessage::Durable(Message::Known(Box::new(KnownMessage::Developer {
            id: MessageId::new(id),
            content: blocks,
            source: MessageSource::Other(RawObject::new()),
            extra: RawObject::new(),
        })))
    }

    fn tool_schema(name: &str, deferred: bool) -> ToolSchema {
        ToolSchema {
            defer_loading: deferred.then_some(TrueOnly),
            name: name.to_owned(),
            description: String::new(),
            parameters: RawObject::new(),
        }
    }

    fn tool_update_message() -> RequestMessage {
        developer_message(
            "dev-1",
            vec![
                ContentBlock::Known(KnownContentBlock::ToolAddition {
                    tool_name: "paint".to_owned(),
                }),
                ContentBlock::Known(KnownContentBlock::ToolRemoval {
                    tool_name: "search".to_owned(),
                }),
            ],
        )
    }

    fn update_history() -> ToolHistory {
        ToolHistory {
            tools: vec![tool_schema("search", false)],
            updates: vec![fr_wire::ToolHistoryUpdate {
                message_id: MessageId::new("dev-1"),
                additions: vec![tool_schema("paint", false)],
            }],
        }
    }

    fn developer_content(messages: &[RequestMessage]) -> Vec<ContentBlock> {
        messages
            .iter()
            .find(|message| message.role_name() == Some("developer"))
            .map(RequestMessage::content_blocks)
            .unwrap_or_default()
    }

    /// The bound scenario: with the route declaring `addition-only`, a
    /// removed tool leaves the declaration list and its removal message is
    /// stripped; the `in-history` route keeps both.
    #[test]
    fn project_tool_updates_addition_only_drops_inactive() {
        let messages = vec![user_message(vec![]), tool_update_message()];
        let active = [tool_schema("paint", false)];
        let history = update_history();
        let projected = project_tool_updates(
            &messages,
            Some(&active),
            Some(ToolUpdate::AdditionOnly),
            Some(&history),
        );
        let names: Vec<&str> = projected
            .tools
            .as_ref()
            .expect("declarations")
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, vec!["paint"]);
        assert_eq!(developer_content(&projected.messages).len(), 1);
        let in_history = project_tool_updates(
            &messages,
            Some(&active),
            Some(ToolUpdate::InHistory),
            Some(&history),
        );
        let names: Vec<&str> = in_history
            .tools
            .as_ref()
            .expect("declarations")
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, vec!["search", "paint"]);
        assert_eq!(developer_content(&in_history.messages).len(), 2);
    }

    /// The bound scenario: `in-history` keeps a removed tool's definition
    /// (disabling does not discard) and retains its removal message.
    #[test]
    fn project_tool_updates_in_history_keeps_removals() {
        let history = ToolHistory {
            tools: vec![tool_schema("search", false)],
            updates: vec![fr_wire::ToolHistoryUpdate {
                message_id: MessageId::new("dev-1"),
                additions: vec![],
            }],
        };
        let messages = vec![developer_message(
            "dev-1",
            vec![ContentBlock::Known(KnownContentBlock::ToolRemoval {
                tool_name: "search".to_owned(),
            })],
        )];
        let projected = project_tool_updates(
            &messages,
            Some(&[]),
            Some(ToolUpdate::InHistory),
            Some(&history),
        );
        let names: Vec<&str> = projected
            .tools
            .as_ref()
            .expect("declarations")
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(names, vec!["search"]);
        let blocks = developer_content(&projected.messages);
        assert!(matches!(
            blocks.as_slice(),
            [ContentBlock::Known(KnownContentBlock::ToolRemoval { .. })]
        ));
    }

    /// The bound scenario: the budget shortfall names the oldest-occurrence
    /// count and never offloads on its own; the byte-quantum boundary is
    /// asymmetric exactly as dsh pins it.
    #[test]
    fn required_image_offload_counts_not_evicts() {
        let image = || {
            ContentBlock::Known(KnownContentBlock::Image {
                attachment: image_ref(None),
                offloaded: None,
            })
        };
        let messages = vec![user_message(vec![image(), image(), image()])];
        let before = messages[0].content_blocks();
        let budget = ImageRequestBudget {
            representation: ImageRepresentation::Raw,
            max_bytes: Some(100),
            max_images: None,
            byte_quantum: Some(1),
            count_quantum: None,
        };
        let count = required_image_offload(&messages, &budget, |attachment| attachment.bytes);
        // 300 total bytes against a 100-byte ceiling: 200 excess bytes, one
        // per image, so the scan stops after the second oldest occurrence.
        assert_eq!(count, 2);
        assert_eq!(messages[0].content_blocks(), before);
        let coarse = ImageRequestBudget {
            byte_quantum: Some(2),
            ..budget
        };
        // With quantum 2 the byte target needs 200 removed before the
        // comparison (`>`), so all three occurrences answer the count.
        assert_eq!(
            required_image_offload(&messages, &coarse, |attachment| attachment.bytes),
            3
        );
    }

    /// The route-owned substitute texts are protocol-facing and pinned
    /// verbatim; the file projection swaps every file occurrence.
    #[test]
    fn projection_texts_pin_dsh_wording() {
        let named = image_ref(Some("pic"));
        assert_eq!(
            text_only_image_text(&named),
            "[image omitted because this model accepts text only; attachment sha256:abcd1234]"
        );
        assert_eq!(
            offloaded_image_text(&named, None),
            "[image omitted to fit request image limits; \"pic\" (sha256:abcd1234ef567890). No local normalized image path is available; ask the user to attach it again if needed.]"
        );
        assert_eq!(
            request_image_handle_text(&named, (64, 64), None),
            "Image \"pic\" (sha256:abcd1234ef567890); request preview 64x64px. It may be resized or re-encoded; source dimensions, format, and byte size may differ."
        );
        let access = ImageAttachmentAccess {
            readonly_path: "/ro/pic.png".to_owned(),
        };
        assert_eq!(
            request_image_handle_text(&named, (32, 32), Some(&access)),
            "Image \"pic\" (sha256:abcd1234ef567890); request preview 32x32px. Normalized copy (read-only; may be resized or re-encoded): \"/ro/pic.png\" (64x64px, image/png). Source dimensions, format, and byte size may differ. Copy to a writable path ending in .png before editing."
        );
        let file = file_ref();
        assert_eq!(
            file_handle_text(&file, None),
            "[File \"report.txt\" (11 bytes, sha256:abcd1234) was uploaded, but the current execution environment cannot access a readable path. Report that limitation if its contents are needed; do not claim to have read it.]"
        );
        assert_eq!(
            file_handle_text(&file, Some("/saved/report.txt")),
            "[File \"report.txt\" (11 bytes, sha256:abcd1234): verbatim read-only copy saved at \"/saved/report.txt\". Read that path with your file tools when its contents are needed; copy it to a writable location before modifying it. When delegating file work, include this saved path in the delegation prompt; only subagents sharing this execution environment can read it.]"
        );
        let messages = vec![user_message(vec![ContentBlock::Known(
            KnownContentBlock::File { attachment: file },
        )])];
        let projected = project_files_to_text(&messages, &|_| None);
        let ContentBlock::Known(KnownContentBlock::Text { text }) =
            &projected[0].content_blocks()[0]
        else {
            panic!("file must project to text");
        };
        assert!(text.contains("cannot access a readable path"));
    }
}
