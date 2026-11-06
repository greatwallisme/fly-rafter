// Rust guideline compliant 2026-11-06
//! The canonical selection of a child's final assistant output (dsh
//! `subagent/src/assistant-output.ts` 1:1): the last non-empty
//! assistant message wins; without one, the accumulated streamed text
//! fallback; without either, nothing.
//!
//! Selection is independent of the run's stop reason.

use serde_json::{Value, json};

/// Extends the streamed fallback with one text piece (dsh
/// `AssistantOutputFold.pushText`).
pub fn push_text(partial: &mut String, text: &str) {
    if !text.is_empty() {
        partial.push_str(text);
    }
}

/// The joined stream text of one assistant event's `stream` field
/// (dsh `joinAssistantStreamText`): concatenated text deltas.
#[must_use]
pub fn stream_text(stream: &Value) -> String {
    let mut joined = String::new();
    if let Some(chunks) = stream.as_array() {
        for chunk in chunks {
            if let Some(text) = chunk
                .as_object()
                .and_then(|object| object.get("text"))
                .and_then(Value::as_str)
            {
                joined.push_str(text);
            }
        }
    }
    joined
}

/// Applies the selection rule to one event slice (dsh
/// `finalAssistantOutput`).
#[must_use]
pub fn final_assistant_output(events: &[Value]) -> Option<Vec<Value>> {
    let mut message: Option<Vec<Value>> = None;
    let mut partial = String::new();
    for event in events {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_type == "assistant/message" {
            let content = event
                .get("data")
                .and_then(|data| data.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array);
            if let Some(content) = content {
                if !content.is_empty() {
                    message = Some(content.clone());
                }
            }
            if let Some(stream) = event.get("data").and_then(|data| data.get("stream")) {
                push_text(&mut partial, &stream_text(stream));
            }
        } else if event_type == "assistant/attempt" {
            if let Some(stream) = event.get("data").and_then(|data| data.get("stream")) {
                push_text(&mut partial, &stream_text(stream));
            }
        }
    }
    if let Some(content) = message {
        return Some(content);
    }
    if !partial.is_empty() {
        return Some(vec![json!({ "type": "text", "text": partial })]);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::final_assistant_output;
    use serde_json::json;

    #[test]
    fn the_last_nonempty_assistant_message_wins() {
        let events = [
            json!({ "type": "assistant/attempt", "data": { "stream": [{ "text": "draft" }] } }),
            json!({ "type": "assistant/message", "data": { "message": { "content": [
                { "type": "text", "text": "first" }
            ] }, "stream": [] } }),
            // An empty-content message (usage-only) does not replace it.
            json!({ "type": "assistant/message", "data": { "message": { "content": [] }, "stream": [] } }),
            json!({ "type": "assistant/message", "data": { "message": { "content": [
                { "type": "text", "text": "final" }
            ] }, "stream": [] } }),
        ];
        let output = final_assistant_output(&events).expect("some output");
        assert_eq!(output, vec![json!({ "type": "text", "text": "final" })]);
    }

    #[test]
    fn the_streamed_text_is_the_no_message_fallback() {
        let events = [
            json!({ "type": "assistant/attempt", "data": { "stream": [
                { "text": "partial " }, { "text": "answer" }
            ] } }),
            json!({ "type": "assistant/message", "data": { "message": { "content": [] }, "stream": [
                { "text": "!" }
            ] } }),
        ];
        let output = final_assistant_output(&events).expect("some output");
        assert_eq!(
            output,
            vec![json!({ "type": "text", "text": "partial answer!" })]
        );
    }

    #[test]
    fn no_message_and_no_stream_selects_nothing() {
        let events = [
            json!({ "type": "turn/start" }),
            json!({ "type": "assistant/message", "data": { "message": { "content": [] }, "stream": [] } }),
        ];
        assert!(final_assistant_output(&events).is_none());
    }
}
