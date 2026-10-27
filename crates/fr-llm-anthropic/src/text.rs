//! Text extraction over message content (pi-ai `utils/text.ts`).
//!
//! The fr system vocabulary carries no `sections` map (fr-wire
//! `KnownMessage::System` is `{id, content, source, extra}`), so
//! `getSystemMessageText` reduces to the content join; the
//! `renderSystemMessageUpdate` framing is dead for fr-llm-openai because
//! Q74-B pins `supportsMidConvoSystemMessages` to `false`, which collapses
//! every system message into the leading one before conversion.

use fr_wire::{ContentBlock, KnownContentBlock};

/// Extract and join text from message content (`contentText`).
#[must_use]
pub fn content_text(content: &[ContentBlock], separator: &str) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Known(KnownContentBlock::Text { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(separator)
}

/// Render a system message as its prompt text (`getSystemMessageText`
/// without the absent sections face).
#[must_use]
pub fn system_message_text(content: &[ContentBlock]) -> String {
    let text = content_text(content, "\n");
    if text.is_empty() { String::new() } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fr_wire::RawObject;
    use serde_json::json;

    fn block(text: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: text.to_owned(),
        })
    }

    fn other(value: serde_json::Value) -> ContentBlock {
        ContentBlock::Other(serde_json::from_value::<RawObject>(value).expect("object"))
    }

    /// Text blocks join on the separator; empty parts drop in the system
    /// rendering, and non-text blocks are skipped (`contentText` filter).
    #[test]
    fn joins_text_and_skips_other_blocks() {
        let content = vec![block("alpha"), other(json!({"type": "odd"})), block("beta")];
        assert_eq!(content_text(&content, "\n"), "alpha\nbeta");
        assert_eq!(system_message_text(&content), "alpha\nbeta");
        assert_eq!(system_message_text(&[]), "");
        assert_eq!(system_message_text(&[other(json!({"type": "odd"}))]), "");
    }
}
