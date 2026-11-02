//! Fixed-density heuristic estimator (dsh `token-meter/estimate.ts` 1:1):
//! the pricing the meter service and the pure folds share, so both
//! surfaces price identical content to identical numbers.
//!
//! All lengths are UTF-16 code-unit counts: the dsh estimator reads JS
//! `.length` (and `JSON.stringify(..).length`), which counts UTF-16
//! code units, not bytes and not Unicode code points.

#![forbid(unsafe_code)]

use fr_wire::content::{ContentBlock, KnownContentBlock};
use fr_wire::event::EpochHeader;
use fr_wire::message::Message;

/// Fixed text-density estimate used until exact tokenization is needed.
const CHARS_PER_TOKEN: u64 = 4;

/// Per-block structural overhead for JSON framing and type tags.
pub(crate) const BLOCK_OVERHEAD: u64 = 4;

/// Role-field framing overhead added to every priced message.
pub const ROLE_OVERHEAD: u64 = 4;

/// The JS `.length` of one string: UTF-16 code-unit count.
#[must_use]
pub fn utf16_len(text: &str) -> u64 {
    u64::try_from(text.chars().map(char::len_utf16).sum::<usize>()).unwrap_or(u64::MAX)
}

/// `Math.ceil(length / 4)` over the UTF-16 length.
const fn ceil_div_4(length: u64) -> u64 {
    length.div_ceil(CHARS_PER_TOKEN)
}

/// Structural JSON price of one block outside the typed pricing arms
/// (dsh `estimateStructuralBlock`).
///
/// The fixed heuristic covers merge-extended blocks and image
/// references, whose request price is route-owned rather than fixed. An
/// image offload mark does not change this reference-only heuristic;
/// route pricing owns the substitute texts.
#[must_use]
pub fn estimate_structural_block(block: &ContentBlock) -> u64 {
    let serialized = match block {
        ContentBlock::Known(KnownContentBlock::Image {
            attachment,
            offloaded: Some(_),
        }) => {
            // The reference JSON prices the block minus the offloaded
            // mark (the dsh rest-destructure removal), so the known
            // image shape rebuilds with the mark absent.
            let reference = KnownContentBlock::Image {
                attachment: attachment.clone(),
                offloaded: None,
            };
            serde_json::to_string(&reference).unwrap_or_default()
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    BLOCK_OVERHEAD.saturating_add(ceil_div_4(utf16_len(&serialized)))
}

/// Price content blocks recursively under the fixed density heuristic
/// (dsh `estimateContent`).
#[must_use]
pub fn estimate_content(blocks: &[ContentBlock]) -> u64 {
    let mut tokens = 0u64;
    for block in blocks {
        tokens = tokens.saturating_add(match block {
            ContentBlock::Known(
                KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text },
            ) => ceil_div_4(utf16_len(text)).saturating_add(BLOCK_OVERHEAD),
            ContentBlock::Known(KnownContentBlock::ToolCall {
                name, arguments, ..
            }) => ceil_div_4(utf16_len(name))
                .saturating_add(ceil_div_4(utf16_len(arguments)))
                .saturating_add(BLOCK_OVERHEAD),
            // The content map is merge-extensible; unknown blocks (and
            // image references, whose request price is route-owned)
            // retain a conservative structural JSON price under the
            // fixed heuristic.
            other => estimate_structural_block(other),
        });
    }
    tokens
}

/// The single-text-block content price (the dsh
/// `estimateContent([{ type: 'text', text }])` shape route pricing uses
/// to price model-visible occurrence texts).
#[must_use]
pub fn estimate_text_block(text: &str) -> u64 {
    ceil_div_4(utf16_len(text)).saturating_add(BLOCK_OVERHEAD)
}

/// Price the rendered system prompt (dsh `estimateSystemMessage`).
///
/// Adapters serialize the `system/message` surface node as a plain
/// string, so the price is text density plus role framing with no
/// per-block overhead; 0 for empty content ("no system prompt").
#[must_use]
pub fn estimate_system_message(message: &Message) -> u64 {
    match message {
        fr_wire::message::Message::Known(known) => {
            let content = known.content();
            if content.is_empty() {
                return 0;
            }
            let mut characters = 0u64;
            for block in content {
                characters = characters.saturating_add(match block {
                    ContentBlock::Known(
                        KnownContentBlock::Text { text } | KnownContentBlock::Reasoning { text },
                    ) => utf16_len(text),
                    other => utf16_len(&serde_json::to_string(other).unwrap_or_default()),
                });
            }
            ceil_div_4(characters).saturating_add(ROLE_OVERHEAD)
        }
        fr_wire::message::Message::Other(raw) => estimate_raw_system(raw),
    }
}

/// Heuristically price one model-visible message (dsh
/// `estimateMessage`).
///
/// Content and role-framing tokens under the fixed heuristic; a
/// system-role message prices as [`estimate_system_message`]. The
/// verbatim unknown-message shape prices through the same arms over
/// its raw JSON.
#[must_use]
pub fn estimate_message(message: &Message) -> u64 {
    if is_system_role(message) {
        return estimate_system_message(message);
    }
    match message {
        fr_wire::message::Message::Known(known) => {
            estimate_content(known.content()).saturating_add(ROLE_OVERHEAD)
        }
        fr_wire::message::Message::Other(raw) => {
            estimate_raw_content(raw).saturating_add(ROLE_OVERHEAD)
        }
    }
}

/// Price the tool-schema part of a canonical request envelope (dsh
/// `estimateToolsTokens`).
///
/// The envelope's only priced field, since the system prompt is a
/// surface node; 0 when absent or empty.
#[must_use]
pub fn estimate_tools_tokens(header: Option<&EpochHeader>) -> u64 {
    let Some(tools) = header.and_then(|header| header.tools.as_ref()) else {
        return 0;
    };
    if tools.is_empty() {
        return 0;
    }
    let serialized = serde_json::to_string(tools).unwrap_or_default();
    ceil_div_4(utf16_len(&serialized)).saturating_add(BLOCK_OVERHEAD)
}

/// The system prompt price of a verbatim unknown message: the same
/// density arms over the raw blocks.
fn estimate_raw_system(raw: &serde_json::Map<String, serde_json::Value>) -> u64 {
    let content = raw.get("content").and_then(|content| content.as_array());
    let Some(blocks) = content else {
        return 0;
    };
    if blocks.is_empty() {
        return 0;
    }
    let mut characters = 0u64;
    for block in blocks {
        let text = block.get("type").and_then(|kind| kind.as_str()) == Some("text");
        characters = characters.saturating_add(if text {
            utf16_len(
                block
                    .get("text")
                    .and_then(|text| text.as_str())
                    .unwrap_or(""),
            )
        } else {
            utf16_len(&serde_json::to_string(block).unwrap_or_default())
        });
    }
    ceil_div_4(characters).saturating_add(ROLE_OVERHEAD)
}

/// The message role check (the dsh `message.role === 'system'` arm).
fn is_system_role(message: &Message) -> bool {
    match message {
        fr_wire::message::Message::Known(known) => known.role_name() == "system",
        fr_wire::message::Message::Other(raw) => {
            raw.get("role").and_then(|role| role.as_str()) == Some("system")
        }
    }
}

/// The content price of a verbatim unknown message (dsh
/// `estimateContent` over runtime objects).
fn estimate_raw_content(raw: &serde_json::Map<String, serde_json::Value>) -> u64 {
    raw.get("content")
        .and_then(|content| content.as_array())
        .map_or(0, |blocks| blocks.iter().map(estimate_raw_block).sum())
}

/// Price one raw content block through the same type arms.
fn estimate_raw_block(block: &serde_json::Value) -> u64 {
    match block.get("type").and_then(|kind| kind.as_str()) {
        Some("text" | "reasoning") => {
            let text = block
                .get("text")
                .and_then(|text| text.as_str())
                .unwrap_or("");
            ceil_div_4(utf16_len(text)).saturating_add(BLOCK_OVERHEAD)
        }
        Some("tool-call") => {
            let name = block
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("");
            let arguments = block
                .get("arguments")
                .and_then(|arguments| arguments.as_str())
                .unwrap_or("");
            ceil_div_4(utf16_len(name))
                .saturating_add(ceil_div_4(utf16_len(arguments)))
                .saturating_add(BLOCK_OVERHEAD)
        }
        Some("image") => {
            // The reference price strips the offloaded mark (the dsh
            // rest-destructure removal applies to any image object).
            let mut reference = block.clone();
            if let serde_json::Value::Object(ref mut map) = reference {
                map.remove("offloaded");
            }
            BLOCK_OVERHEAD.saturating_add(ceil_div_4(utf16_len(
                &serde_json::to_string(&reference).unwrap_or_default(),
            )))
        }
        _ => BLOCK_OVERHEAD.saturating_add(ceil_div_4(utf16_len(
            &serde_json::to_string(block).unwrap_or_default(),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_lengths_count_code_units_not_bytes() {
        // ASCII: one byte, one code unit.
        assert_eq!(utf16_len("abc"), 3);
        // A CJK character: three UTF-8 bytes, one UTF-16 code unit.
        assert_eq!(utf16_len("汉"), 1);
        // An astral emoji: four UTF-8 bytes, two UTF-16 code units.
        assert_eq!(utf16_len("🎉"), 2);
        let text = String::from("汉a🎉");
        assert_eq!(utf16_len(&text), 1 + 1 + 2);
    }

    #[test]
    fn estimate_message_pins_the_density_math() {
        let message =
            fr_wire::message::Message::Known(Box::new(fr_wire::message::KnownMessage::User {
                id: fr_wire::ids::MessageId::new("m-1"),
                content: vec![ContentBlock::Known(KnownContentBlock::Text {
                    text: "abcdefgh".to_owned(), // 8 units -> ceil(8/4)=2 + 4
                })],
                source: fr_wire::message::MessageSource::Known(
                    fr_wire::message::KnownMessageSource::User {
                        extra: fr_wire::content::RawObject::new(),
                    },
                ),
                extra: fr_wire::content::RawObject::new(),
            }));
        assert_eq!(
            estimate_message(&message),
            2 + BLOCK_OVERHEAD + ROLE_OVERHEAD
        );
    }

    #[test]
    fn estimate_tools_tokens_pins_absent_and_present() {
        let header = EpochHeader {
            config: fr_wire::call_config::CallConfig::default(),
            adapter_defaults: None,
            tools: None,
        };
        assert_eq!(estimate_tools_tokens(Some(&header)), 0);
        let tools = vec![fr_wire::call_config::ToolSchema {
            defer_loading: None,
            name: "read".to_owned(),
            description: "Reads.".to_owned(),
            parameters: fr_wire::content::RawObject::new(),
        }];
        let serialized = serde_json::to_string(tools.as_slice()).unwrap_or_default();
        let with_tools = EpochHeader {
            tools: Some(tools),
            ..header
        };
        let expected = BLOCK_OVERHEAD + ceil_div_4(utf16_len(&serialized));
        assert_eq!(estimate_tools_tokens(Some(&with_tools)), expected);
    }
}
