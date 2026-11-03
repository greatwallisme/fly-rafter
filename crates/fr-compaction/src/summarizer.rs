//! Default one-shot summarization vocabulary and durable checkpoint framing
//! (dsh compaction-basic/src/summarizer.ts 1:1).
//!
//! The framing constants are script-extracted
//! (`scripts/extract-compaction-prompts`). The `summarizeWithLlm` body — the
//! LLM-seam consumer — assembles with the engine shell at 17f; the pure face
//! (types, framing, finish mapping, and the text-only projection) is
//! complete here.

use fr_wire::call_config::FinishReason;
use fr_wire::content::KnownContentBlock;
use fr_wire::{ContentBlock, Message, ToolSchema};

use crate::prompts::{CHECKPOINT_PREAMBLE, SUMMARY_CLOSE_TAG, SUMMARY_OPEN_TAG};

/// The replayed conversation surface the summarizer condenses (dsh
/// `SummarizationInput`).
///
/// Reproducing the last routed request's system prompt, tools, and leading
/// messages verbatim lets the auxiliary call reuse the provider's warm
/// prefix cache; the trailing compaction instruction is then the only novel
/// input.
#[derive(Debug, Clone, PartialEq)]
pub struct SummarizationInput {
    /// The conversation's tool schemas, reused for prefix-cache alignment;
    /// absent when the request carried none.
    pub tools: Option<Vec<ToolSchema>>,
    /// The derived system head, when present, followed by the shadowed
    /// region in surface order.
    pub messages: Vec<Message>,
}

/// Safe summary content plus the exact auxiliary call envelope recorded
/// with it (dsh `SummaryResult`).
///
/// The dsh discriminated union collapses to the optional pair — the wire
/// faces are identical up to the optional key.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryResult {
    /// Safe text-only model output.
    pub summary: Vec<ContentBlock>,
    /// Complete provider output before the text-only summary projection.
    pub raw_output: Option<Vec<ContentBlock>>,
    /// Marks the result the product of exactly one call through this
    /// context's LLM seam; absent for an unmarked template, remote, or other
    /// summarizer.
    pub llm_stream_call: bool,
    /// The provider route that wrote the summary.
    pub provider: String,
    /// The model that wrote the summary.
    pub model: String,
    /// The generation cap the summarize call sent, when one applied.
    pub max_tokens: Option<u64>,
    /// Provider-reported usage for the summarization request, when emitted.
    pub usage: Option<fr_wire::call_config::TokenUsage>,
}

/// Wrap raw summary blocks in the durable checkpoint framing (dsh
/// `frameSummary`).
#[must_use]
pub fn frame_summary(summary: &[ContentBlock]) -> Vec<ContentBlock> {
    let mut framed = Vec::with_capacity(summary.len() + 2);
    framed.push(text_block(&format!(
        "{CHECKPOINT_PREAMBLE}\n\n{SUMMARY_OPEN_TAG}"
    )));
    framed.extend(summary.iter().cloned());
    framed.push(text_block(SUMMARY_CLOSE_TAG));
    framed
}

/// Map a terminal summarization finish to its fail-closed error (dsh
/// `finishError`).
///
/// `error` and `aborted` finishes carry the failure facts verbatim;
/// `max-tokens` maps to the fixed truncated-checkpoint diagnostic with the
/// `MAX_TOKENS` code; every other finish succeeded.
#[must_use]
pub fn finish_error(finish: &FinishReason) -> Option<CompactionFailure> {
    match finish {
        FinishReason::Known(fr_wire::call_config::FinishReasonKind::MaxTokens) => {
            Some(CompactionFailure::Llm {
                message: "summarization truncated at the token cap (incomplete checkpoint)"
                    .to_owned(),
                code: Some("MAX_TOKENS".to_owned()),
            })
        }
        FinishReason::Known(
            fr_wire::call_config::FinishReasonKind::Aborted { failure }
            | fr_wire::call_config::FinishReasonKind::Error { failure },
        ) => Some(CompactionFailure::Llm {
            message: failure.message.clone(),
            code: Some(failure.code.clone()),
        }),
        _ => None,
    }
}

/// Reject visual output and keep only text before synthesizing a user
/// message (dsh `summaryText`).
///
/// # Errors
///
/// Returns the `UNSUPPORTED_CONTENT` failure when any block is an image.
pub fn summary_text(blocks: &[ContentBlock]) -> Result<Vec<ContentBlock>, CompactionFailure> {
    if blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::Known(KnownContentBlock::Image { .. })))
    {
        return Err(CompactionFailure::Llm {
            message: "compaction summary cannot contain image output".to_owned(),
            code: Some("UNSUPPORTED_CONTENT".to_owned()),
        });
    }
    Ok(blocks
        .iter()
        .filter(|block| matches!(block, ContentBlock::Known(KnownContentBlock::Text { .. })))
        .cloned()
        .collect())
}

fn text_block(text: &str) -> ContentBlock {
    ContentBlock::Known(KnownContentBlock::Text {
        text: text.to_owned(),
    })
}

/// One opaque failure flowing through a compaction transaction (dsh passes
/// the thrown value through; fr classifies it so the recovery arm and the
/// manual classifier can match on it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionFailure {
    /// The selected span or whole surface changed under the transaction
    /// (dsh `SurfaceChangedError`); the message is the verbatim diagnostic.
    SurfaceChanged(String),
    /// An LLM-seam or other coded failure: the message plus the provider
    /// error code when one applies (`MAX_TOKENS`, `UNSUPPORTED_CONTENT`,
    /// `CONTEXT_WINDOW_EXCEEDED`, `IMAGE_OFFLOAD_REQUIRED`, ...).
    Llm {
        /// The verbatim failure message.
        message: String,
        /// The provider error code, when one applies.
        code: Option<String>,
    },
    /// Any other backend diagnostic, carried verbatim.
    Other(String),
}

impl CompactionFailure {
    /// The verbatim diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::SurfaceChanged(message) | Self::Llm { message, .. } | Self::Other(message) => {
                message
            }
        }
    }
}

impl std::fmt::Display for CompactionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for CompactionFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::COMPACTION_INSTRUCTION;
    use fr_wire::call_config::FinishReasonKind;

    fn text(value: &str) -> ContentBlock {
        ContentBlock::Known(KnownContentBlock::Text {
            text: value.to_owned(),
        })
    }

    #[test]
    fn frames_the_checkpoint_with_the_extracted_brackets() {
        let framed = frame_summary(&[text("body")]);
        assert_eq!(framed.len(), 3);
        assert_eq!(
            framed[0],
            text(&format!("{CHECKPOINT_PREAMBLE}\n\n{SUMMARY_OPEN_TAG}"))
        );
        assert_eq!(framed[1], text("body"));
        assert_eq!(framed[2], text(SUMMARY_CLOSE_TAG));
        assert_eq!(SUMMARY_OPEN_TAG, "<compacted-summary>");
        assert_eq!(SUMMARY_CLOSE_TAG, "</compacted-summary>");
        assert!(
            CHECKPOINT_PREAMBLE
                .starts_with("This is an automatically generated checkpoint condensing")
        );
        assert!(COMPACTION_INSTRUCTION.contains("## Primary Request and Intent"));
        assert!(COMPACTION_INSTRUCTION.contains("contains a <compacted-summary> block"));
        assert!(COMPACTION_INSTRUCTION.ends_with("under the same structure."));
    }

    #[test]
    fn maps_finish_faces_to_fail_closed_errors() {
        let max_tokens = FinishReason::Known(FinishReasonKind::MaxTokens);
        assert_eq!(
            finish_error(&max_tokens).expect("max tokens"),
            CompactionFailure::Llm {
                message: "summarization truncated at the token cap (incomplete checkpoint)"
                    .to_owned(),
                code: Some("MAX_TOKENS".to_owned()),
            }
        );
        let aborted = FinishReason::Known(FinishReasonKind::Aborted {
            failure: fr_wire::call_config::Failure {
                message: "cancelled".to_owned(),
                code: "ABORTED".to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: None,
            },
        });
        assert_eq!(
            finish_error(&aborted).expect("aborted"),
            CompactionFailure::Llm {
                message: "cancelled".to_owned(),
                code: Some("ABORTED".to_owned()),
            }
        );
        let stop = FinishReason::Known(FinishReasonKind::Stop);
        assert_eq!(finish_error(&stop), None);
    }

    #[test]
    fn rejects_image_output_and_keeps_only_text() {
        let image = ContentBlock::Known(KnownContentBlock::Image {
            attachment: fr_wire::ImageAttachmentRef {
                attachment_id: fr_wire::AttachmentId::new("a"),
                media_type: fr_wire::ImageMediaType::Png,
                bytes: 3,
                width: 1,
                height: 1,
                name: Some("a.png".to_owned()),
                original_dimensions: None,
            },
            offloaded: None,
        });
        let error = summary_text(&[text("head"), image, text("tail")]).expect_err("image");
        assert_eq!(
            error,
            CompactionFailure::Llm {
                message: "compaction summary cannot contain image output".to_owned(),
                code: Some("UNSUPPORTED_CONTENT".to_owned()),
            }
        );
        let reasoning = ContentBlock::Known(KnownContentBlock::Reasoning {
            text: "thinking".to_owned(),
        });
        let kept = summary_text(&[text("head"), reasoning, text("tail")]).expect("kept");
        assert_eq!(kept, vec![text("head"), text("tail")]);
    }
}
