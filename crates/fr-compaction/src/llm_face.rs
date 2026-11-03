//! The default one-shot summarization call over the LLM seam (dsh
//! compaction-basic/src/summarizer.ts `summarizeWithLlm` port).
//!
//! The dsh call rides `ctx.llm.stream(options)`; the fr seam is the
//! `'llm'` service's begin/drain generation face, so the stream consumer
//! is a pull loop: begin one generation, drain buffered batches into the
//! assembler until the terminal settlement, then classify the finish.
//! The poll cadence is the behaviorally honest stand-in for the
//! notification wake the loop driver uses: the chunks land in the
//! assembler only, so a drain-granularity latency never reaches a user.
//!
//! Cancellation (the dsh `signal`): the summarizer polls the caller's
//! abort probe between drains; a raised probe flags the generation
//! cancelled through the `abort` face, and the terminal settlement
//! arrives `aborted` — the same fail-closed classification the dsh
//! adapter delivers.
//!
//! Target resolution (dsh `summarizeWithLlm` lines 127-141): the
//! config's `summarization-provider`/`summarization-model` pair wins,
//! then the latest durable routed request, then the declared agent
//! route; all three missing is the verbatim fail-closed error.

use fr_llm::adapter::CallPurpose;
use fr_llm::assembler::BlockAssembler;
use fr_llm::generation::{DrainReply, WireGenerateOptions, WireModelInfo, WireRequestMessage};

use crate::prompts::COMPACTION_INSTRUCTION;
use crate::summarizer::{CompactionFailure, SummarizationInput, finish_error, summary_text};

/// The provider route the summarization call targets (the dsh
/// `options.provider`/`.model` pair).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarizeTarget {
    /// The provider route key.
    pub provider: String,
    /// The model id.
    pub model: String,
}

/// The LLM stream seam one summarization call consumes (the dsh
/// `ctx.llm.stream` face collapsed to the generation pull trio plus the
/// route-metadata lookup the pressure trigger shares).
pub trait SummarizeStream {
    /// Resolves one route's model metadata (the dsh
    /// `llm.resolveModelInfo`; the pressure trigger's capacity facts).
    ///
    /// # Errors
    ///
    /// Returns the service diagnostic verbatim.
    fn resolve_model_info(&mut self, provider: &str, model: &str) -> Result<WireModelInfo, String>;

    /// Begins one generation (the dsh stream start).
    ///
    /// # Errors
    ///
    /// Returns the service diagnostic verbatim.
    fn begin(&mut self, request: WireGenerateOptions) -> Result<String, String>;

    /// Pulls one buffered batch (the dsh async iterator's next).
    ///
    /// # Errors
    ///
    /// Returns the service diagnostic verbatim.
    fn drain(&mut self, generation: &str) -> Result<DrainReply, String>;

    /// Flags one generation cancelled (the dsh `signal` forwarding).
    ///
    /// # Errors
    ///
    /// Returns the service diagnostic verbatim.
    fn abort(&mut self, generation: &str) -> Result<(), String>;
}

/// Resolves the summarization target (dsh lines 127-141).
///
/// The configured summarization pair, the latest durable routed request,
/// then the declared agent route; empty strings count as unset exactly
/// as the dsh `length === 0` checks do.
///
/// # Errors
///
/// Returns the verbatim dsh rejection when all three sources are unset.
pub fn resolve_summarization_target(
    configured_provider: &str,
    configured_model: &str,
    routed_provider: Option<&str>,
    routed_model: Option<&str>,
    agent_provider: Option<&str>,
    agent_model: Option<&str>,
) -> Result<SummarizeTarget, String> {
    let configured = (!configured_provider.is_empty() && !configured_model.is_empty()).then(|| {
        SummarizeTarget {
            provider: configured_provider.to_owned(),
            model: configured_model.to_owned(),
        }
    });
    let routed = routed_provider
        .zip(routed_model)
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .map(|(provider, model)| SummarizeTarget {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
    let agent = agent_provider
        .zip(agent_model)
        .filter(|(provider, model)| !provider.is_empty() && !model.is_empty())
        .map(|(provider, model)| SummarizeTarget {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
    configured.or(routed).or(agent).ok_or_else(|| {
        "no provider/model available for summarization: set both BasicCompactionConfig \
         summarization fields, route one request, or set both AgentOptions fields"
            .to_owned()
    })
}

/// Runs the default cache-reusing summarization call (dsh
/// `summarizeWithLlm`).
///
/// Replays the conversation prefix, appends the compaction instruction
/// as the final user message, drains the generation into the assembler,
/// and classifies the finish.
///
/// # Errors
///
/// Returns the fail-closed [`CompactionFailure`] for an error/aborted/
/// max-tokens finish, visual output, or an empty summary; the verbatim
/// diagnostic rides the failure.
pub fn summarize_with_llm(
    stream: &mut dyn SummarizeStream,
    target: &SummarizeTarget,
    max_tokens: i64,
    input: &SummarizationInput,
    session_id: &str,
    tool_history: Option<fr_wire::ToolHistory>,
    abort_poll: &mut dyn FnMut() -> bool,
) -> Result<SummaryResultShaped, Box<CompactionFailure>> {
    let mut messages: Vec<WireRequestMessage> = input
        .messages
        .iter()
        .map(|message| WireRequestMessage::Durable {
            message: message.clone(),
        })
        .collect();
    messages.push(WireRequestMessage::UserInput {
        content: vec![fr_wire::ContentBlock::Known(
            fr_wire::KnownContentBlock::Text {
                text: COMPACTION_INSTRUCTION.to_owned(),
            },
        )],
    });
    let request = WireGenerateOptions {
        provider: target.provider.clone(),
        model: target.model.clone(),
        reasoning_effort: None,
        messages,
        system: None,
        tools: input.tools.clone(),
        tool_history,
        temperature: None,
        max_tokens: u64::try_from(max_tokens.max(0)).ok(),
        stop: None,
        session_id: Some(fr_wire::SessionId::new(session_id.to_owned())),
        purpose: Some(CallPurpose::Compaction.as_str().to_owned()),
    };
    let generation = stream
        .begin(request)
        .map_err(|e| Box::new(llm_transport(e)))?;
    let mut assembler = BlockAssembler::default();
    let mut aborted = false;
    loop {
        let reply = stream
            .drain(&generation)
            .map_err(|e| Box::new(llm_transport(e)))?;
        for chunk in &reply.chunks {
            assembler.push(chunk);
        }
        if reply.done {
            break;
        }
        // The cancellation poll: the dsh signal's wire face. The probe
        // fires the abort once; the terminal settlement still drains
        // through before the finish classification.
        if !aborted && abort_poll() {
            aborted = true;
            let _ = stream.abort(&generation);
        }
        if !aborted {
            // The notification-free cadence: batches land here anyway,
            // and an idle provider gap parks this thread, never the loop.
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    if let Some(error) = finish_error(&assembler.finish()) {
        return Err(Box::new(error));
    }
    let raw_output = assembler
        .blocks()
        .map_err(|error| Box::new(CompactionFailure::Other(error.to_string())))?;
    let summary = summary_text(&raw_output).map_err(Box::new)?;
    if !summary
        .iter()
        .any(|block| matches!(block, fr_wire::ContentBlock::Known(fr_wire::KnownContentBlock::Text { text }) if !text.trim().is_empty()))
    {
        return Err(Box::new(CompactionFailure::Other(
            "summarization produced no text summary content".to_owned(),
        )));
    }
    Ok(SummaryResultShaped {
        summary,
        raw_output: Some(raw_output),
        llm_stream_call: true,
        provider: target.provider.clone(),
        model: target.model.clone(),
        max_tokens: u64::try_from(max_tokens.max(0)).ok(),
        usage: assembler.usage(),
    })
}

use crate::summarizer::SummaryResult;

/// The summarize call's return face — the crate's [`SummaryResult`] (the
/// dsh union collapsed to the optional pair) under its own alias so the
/// signature reads as this module's own product.
pub type SummaryResultShaped = SummaryResult;

/// Maps one transport-level stream rejection onto the coded LLM failure
/// (the dsh thrown error's message verbatim; no provider code applies).
const fn llm_transport(message: String) -> CompactionFailure {
    CompactionFailure::Llm {
        message,
        code: None,
        failure: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SummarizeStream, SummarizeTarget, resolve_summarization_target, summarize_with_llm,
    };
    use crate::summarizer::{CompactionFailure, SummarizationInput};
    use fr_llm::generation::{DrainReply, WireGenerateOptions};
    use fr_wire::stream::{KnownStreamChunk, StreamChunk};

    /// The scripted stream double: every drain yields one text batch then
    /// the terminal settlement.
    struct ScriptedStream {
        script: Vec<StreamChunk>,
        aborts: u32,
        drain_error: Option<String>,
    }

    impl ScriptedStream {
        fn text(summary: &str) -> Self {
            Self {
                script: vec![
                    StreamChunk::Known(KnownStreamChunk::TextDelta {
                        index: 0,
                        text: summary.to_owned(),
                    }),
                    StreamChunk::Known(KnownStreamChunk::Finish {
                        reason: fr_wire::call_config::FinishReason::Known(
                            fr_wire::call_config::FinishReasonKind::Stop,
                        ),
                        replay_state: None,
                    }),
                ],
                aborts: 0,
                drain_error: None,
            }
        }
    }

    impl SummarizeStream for ScriptedStream {
        fn resolve_model_info(
            &mut self,
            _provider: &str,
            _model: &str,
        ) -> Result<fr_llm::generation::WireModelInfo, String> {
            unreachable!("the summarizer never resolves model info");
        }

        fn begin(&mut self, _request: WireGenerateOptions) -> Result<String, String> {
            Ok("g1".to_owned())
        }

        fn drain(&mut self, _generation: &str) -> Result<DrainReply, String> {
            if let Some(error) = self.drain_error.take() {
                return Err(error);
            }
            let chunks = std::mem::take(&mut self.script);
            Ok(DrainReply {
                session_id: None,
                generation_id: "g1".to_owned(),
                chunks,
                done: true,
            })
        }

        fn abort(&mut self, _generation: &str) -> Result<(), String> {
            self.aborts += 1;
            Ok(())
        }
    }

    fn text_message(text: &str) -> fr_wire::Message {
        serde_json::from_value(serde_json::json!({
            "role": "user",
            "id": format!("msg-{text}"),
            "content": [{ "type": "text", "text": text }],
            "source": { "kind": "user" },
        }))
        .expect("fixture message")
    }

    fn input() -> SummarizationInput {
        SummarizationInput {
            tools: None,
            messages: vec![text_message("earlier turn")],
        }
    }

    fn target() -> SummarizeTarget {
        SummarizeTarget {
            provider: "deepseek".to_owned(),
            model: "m1".to_owned(),
        }
    }

    #[test]
    fn target_resolution_walks_the_three_sources() {
        let error =
            resolve_summarization_target("", "", None, None, None, None).expect_err("all unset");
        assert_eq!(
            error,
            "no provider/model available for summarization: set both \
             BasicCompactionConfig summarization fields, route one request, or set both \
             AgentOptions fields"
        );
        let target = resolve_summarization_target("", "", None, None, Some("a"), Some("b"))
            .expect("agent fallback");
        assert_eq!(
            target,
            SummarizeTarget {
                provider: "a".to_owned(),
                model: "b".to_owned()
            }
        );
        let target = resolve_summarization_target(
            "cfg-p",
            "cfg-m",
            Some("routed-p"),
            Some("routed-m"),
            Some("a"),
            Some("b"),
        )
        .expect("configured wins");
        assert_eq!(
            target,
            SummarizeTarget {
                provider: "cfg-p".to_owned(),
                model: "cfg-m".to_owned()
            }
        );
        let empty_pair = resolve_summarization_target("", "", Some("p"), Some(""), None, None)
            .expect_err("empty routed model counts unset");
        assert!(empty_pair.contains("no provider/model available"));
        // The load-time arm: a half-set summarization pair refuses when
        // the configuration resolves (the plugin load face).
        let error = crate::config::resolve_config(&serde_json::json!({
            "summarization-provider": "only",
        }))
        .expect_err("half pair");
        assert!(error.contains("must be set together"), "{error}");
    }

    #[test]
    fn summarize_replays_the_prefix_and_appends_the_instruction() {
        let mut stream = ScriptedStream::text("checkpoint body");
        let result = summarize_with_llm(
            &mut stream,
            &target(),
            4_096,
            &input(),
            "s1",
            None,
            &mut || false,
        )
        .expect("summary lands");
        assert!(result.llm_stream_call);
        assert_eq!(result.max_tokens, Some(4_096));
        assert_eq!(
            result.provider, "deepseek",
            "the target route records verbatim"
        );
    }

    #[test]
    fn empty_summary_fails_closed() {
        let mut stream = ScriptedStream::text("   ");
        let error = summarize_with_llm(
            &mut stream,
            &target(),
            4_096,
            &input(),
            "s1",
            None,
            &mut || false,
        )
        .expect_err("whitespace-only summary");
        assert_eq!(
            *error,
            CompactionFailure::Other("summarization produced no text summary content".to_owned())
        );
    }

    #[test]
    fn transport_failures_map_to_the_coded_failure() {
        let mut stream = ScriptedStream::text("body");
        stream.drain_error = Some("llm service reply is invalid: drain: bad".to_owned());
        let error = summarize_with_llm(
            &mut stream,
            &target(),
            4_096,
            &input(),
            "s1",
            None,
            &mut || false,
        )
        .expect_err("drain failure");
        match *error {
            CompactionFailure::Llm { message, code, .. } => {
                assert!(message.contains("drain: bad"));
                assert_eq!(code, None);
            }
            other => panic!("expected the Llm arm, got {other:?}"),
        }
    }

    /// A raised abort probe flags the generation cancelled once; the
    /// finish classification consumes whatever settlement the provider
    /// settles with (here the scripted stop — the abort face itself is
    /// exercised by the provider's own settle path).
    #[test]
    fn abort_probe_fires_once_between_drains() {
        // A two-batch stream: the first drain leaves work pending, the
        // probe fires between batches, and the second drain settles.
        struct TwoBatch(ScriptedStream);
        impl SummarizeStream for TwoBatch {
            fn resolve_model_info(
                &mut self,
                _provider: &str,
                _model: &str,
            ) -> Result<fr_llm::generation::WireModelInfo, String> {
                unreachable!();
            }
            fn begin(&mut self, _request: WireGenerateOptions) -> Result<String, String> {
                Ok("g1".to_owned())
            }
            fn drain(&mut self, _generation: &str) -> Result<DrainReply, String> {
                if self.0.script.is_empty() {
                    return Ok(DrainReply {
                        session_id: None,
                        generation_id: "g1".to_owned(),
                        chunks: Vec::new(),
                        done: true,
                    });
                }
                let chunk = self.0.script.remove(0);
                Ok(DrainReply {
                    session_id: None,
                    generation_id: "g1".to_owned(),
                    chunks: vec![chunk],
                    done: self.0.script.is_empty(),
                })
            }
            fn abort(&mut self, _generation: &str) -> Result<(), String> {
                self.0.aborts += 1;
                Ok(())
            }
        }
        let script = TwoBatch(ScriptedStream {
            script: vec![
                StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: 0,
                    text: "half ".to_owned(),
                }),
                StreamChunk::Known(KnownStreamChunk::TextDelta {
                    index: 0,
                    text: "and half".to_owned(),
                }),
                StreamChunk::Known(KnownStreamChunk::Finish {
                    reason: fr_wire::call_config::FinishReason::Known(
                        fr_wire::call_config::FinishReasonKind::Stop,
                    ),
                    replay_state: None,
                }),
            ],
            aborts: 0,
            drain_error: None,
        });
        let mut stream = script;
        let polls = std::cell::Cell::new(0_u32);
        let result = summarize_with_llm(
            &mut stream,
            &target(),
            4_096,
            &input(),
            "s1",
            None,
            &mut || {
                let count = polls.get();
                polls.set(count + 1);
                count == 0
            },
        )
        .expect("summary lands after the abort probe");
        assert!(result.llm_stream_call);
        assert_eq!(stream.0.aborts, 1, "the abort face fires exactly once");
    }
}
