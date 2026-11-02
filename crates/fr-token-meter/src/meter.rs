//! The replay owner: one service-wide estimator and isolated per-session
//! folds (the dsh `TokenMeter` class body 1:1, without the wire face).
//!
//! Raw anchor facts are captured at the latest successful call; the
//! baseline is derived per measurement so the anchored surface reprices
//! under the same route pricing as the current surface it is compared
//! with. Every fallible step of a fold runs before the replay state
//! mutates, so a malformed event remains unread on every retry instead
//! of half-applying.

#![forbid(unsafe_code)]

use fr_llm::adapter::ImageRequestPrice;
use fr_wire::call_config::TokenUsage;
use fr_wire::event::EpochHeader;
use fr_wire::event::{SessionEvent, SessionEventData};
use serde_json::Value;

use crate::estimate::{ROLE_OVERHEAD, estimate_content, estimate_tools_tokens};
use crate::route_pricing::ImagePriceResolver;
use crate::route_pricing::{FileTextResolver, PricedSurface, price_surface};
use crate::surface_fold::{MeterSurfaceNode, commit_surface_tokens, plan_surface_tokens};
use crate::types::{MeasurementBaseline, TokenMeasurement};

/// The anchor facts captured at the latest successful call (the dsh
/// `MeasurementAnchor`).
#[derive(Debug, Clone)]
pub struct MeasurementAnchor {
    /// The canonical envelope in force when the call was made.
    header: Option<EpochHeader>,
    /// Priced surface immediately before the anchored assistant message
    /// commits.
    nodes: Vec<MeterSurfaceNode>,
    /// Price of the call's provider output.
    assistant_tokens: u64,
    /// Provider usage of the call, when it reported one under a known
    /// header.
    usage: Option<TokenUsage>,
}

/// One session's incremental replay fold (the dsh `ReplayState`).
#[derive(Debug, Clone, Default)]
pub struct ReplayState {
    /// Number of durable events already folded.
    pub consumed_events: u64,
    /// The canonical envelope in force after the last header event.
    pub header: Option<EpochHeader>,
    /// The priced surface in model-visible order.
    pub surface: Vec<MeterSurfaceNode>,
    /// The open `step/start` bounds, when one is open.
    pub step_start: Option<(u64, u64)>,
    /// The latest successful call's anchor.
    pub anchor: Option<MeasurementAnchor>,
}

/// Sum disjoint provider usage buckets without double-counting
/// reasoning output (the dsh `usageTokens`).
#[must_use]
pub fn usage_tokens(usage: &TokenUsage) -> u64 {
    usage
        .input_tokens
        .saturating_add(usage.cache_read_tokens.unwrap_or(0))
        .saturating_add(usage.cache_write_tokens.unwrap_or(0))
        .saturating_add(usage.output_tokens)
}

/// Compare optional envelopes so a headerless estimate can track later
/// surface deltas (the dsh `optionalHeaderEquals`).
#[must_use]
pub fn optional_header_equals(left: Option<&EpochHeader>, right: Option<&EpochHeader>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => header_equals(left, right),
        _ => false,
    }
}

/// Field-wise equality over canonical headers (the dsh `headerEquals`):
/// config, adapter defaults, and tool schemas in order; a tool schema
/// compares through its exact wire JSON.
#[must_use]
pub fn header_equals(a: &EpochHeader, b: &EpochHeader) -> bool {
    if !call_config_equal(&a.config, &b.config) {
        return false;
    }
    let defaults_equal = match (&a.adapter_defaults, &b.adapter_defaults) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.reasoning_effort == right.reasoning_effort && left.max_tokens == right.max_tokens
        }
        _ => false,
    };
    if !defaults_equal {
        return false;
    }
    let empty: [fr_wire::call_config::ToolSchema; 0] = [];
    let at = a.tools.as_deref().unwrap_or(&empty);
    let bt = b.tools.as_deref().unwrap_or(&empty);
    at.len() == bt.len()
        && at.iter().zip(bt.iter()).all(|(left, right)| {
            serde_json::to_string(left).unwrap_or_default()
                == serde_json::to_string(right).unwrap_or_default()
        })
}

/// The dsh `callConfigEquals`: provider, model, and the four control
/// fields (the `LlmCallConfig` face without the agent-loop mark).
fn call_config_equal(
    a: &fr_wire::call_config::CallConfig,
    b: &fr_wire::call_config::CallConfig,
) -> bool {
    a.provider == b.provider
        && a.model == b.model
        && a.controls.reasoning_effort == b.controls.reasoning_effort
        && a.controls.temperature == b.controls.temperature
        && a.controls.max_tokens == b.controls.max_tokens
        && a.controls.stop == b.controls.stop
}

/// Fold one event into the replay state (the dsh `_foldEvent`).
///
/// Every fallible step — surface plan and anchor validation — runs
/// before the replay state mutates, so a malformed event remains unread
/// on every retry instead of half-applying.
///
/// # Errors
///
/// Returns the dsh verbatim texts: a step/start over an open one, a
/// step/end with no matching start, an assistant message with no
/// matching start, and the surface-fold range refusal.
pub fn fold_event(state: &mut ReplayState, event: &SessionEvent) -> Result<(), String> {
    if let SessionEventData::Unknown(unknown) = &event.data
        && unknown.type_name == "image/offload"
    {
        offload_targets(state, &unknown.data);
        return Ok(());
    }
    let mut next_header = state.header.clone();
    let mut next_step_start = state.step_start;
    let mut next_anchor = state.anchor.clone();

    match &event.data {
        SessionEventData::RequestHeader(data) => {
            next_header = Some(fr_session::canonical_header(&data.header));
        }
        SessionEventData::StepStart(data) => {
            if let Some((turn, step)) = state.step_start {
                return Err(format!(
                    "token meter: step/start at seq {} arrived before turn {turn}/step {step} ended",
                    event.seq.get()
                ));
            }
            next_step_start = Some((data.turn, data.step));
        }
        SessionEventData::StepEnd(data) => match state.step_start {
            Some((turn, step)) if turn == data.turn && step == data.step => {
                next_step_start = None;
            }
            _ => {
                return Err(format!(
                    "token meter: step/end at seq {} has no matching step/start event",
                    event.seq.get()
                ));
            }
        },
        _ => {}
    }

    let plan = if fr_session::surface::is_surface_eligible(event) {
        Some(plan_surface_tokens(&state.surface, event)?)
    } else {
        None
    };

    if let SessionEventData::AssistantMessage(data) = &event.data {
        let step_start = state.step_start;
        let matched = step_start.is_some_and(|(turn, step)| turn == data.turn && step == data.step);
        if !matched {
            return Err(format!(
                "token meter: assistant/message at seq {} has no matching step/start event",
                event.seq.get()
            ));
        }
        // assistant/message is surface-mandatory at every append/seed
        // boundary, so the plan is always present here.
        let Some(plan) = &plan else {
            return Err(format!(
                "token meter: assistant/message at seq {} is not a surface event",
                event.seq.get()
            ));
        };
        let event_tokens = plan.tokens;
        // The loop admits prompts and user messages after step/start;
        // retries may replace them before succeeding. Only the
        // pre-assistant surface is priced by this call. Provider output
        // stays separate from durable output rewrites.
        next_anchor = if data.usage.is_some() && next_header.is_some() {
            Some(MeasurementAnchor {
                header: next_header.clone(),
                nodes: state.surface.clone(),
                assistant_tokens: estimate_provider_assistant(&data.stream)?,
                usage: data.usage,
            })
        } else {
            Some(MeasurementAnchor {
                header: next_header.clone(),
                nodes: state.surface.clone(),
                assistant_tokens: event_tokens,
                usage: None,
            })
        };
    }

    state.header = next_header;
    state.step_start = next_step_start;
    if let Some(plan) = plan {
        commit_surface_tokens(&mut state.surface, plan);
    }
    state.anchor = next_anchor;
    Ok(())
}

/// Patch the surface nodes' image occurrences to offloaded (the dsh
/// `image/offload` arm; the decision targets carry per-node image
/// indexes). Idempotent: a re-fold after a later failure re-applies the
/// same marks.
fn offload_targets(state: &mut ReplayState, data: &fr_wire::content::RawObject) {
    let Some(targets) = data.get("targets").and_then(|targets| targets.as_array()) else {
        return;
    };
    for target in targets {
        let Some(seq) = target.get("seq").and_then(Value::as_u64) else {
            continue;
        };
        let Some(indexes) = target.get("imageIndexes").and_then(Value::as_array) else {
            continue;
        };
        let indexes: std::collections::HashSet<u64> =
            indexes.iter().filter_map(Value::as_u64).collect();
        for node in &mut state.surface {
            if node.seq != seq {
                continue;
            }
            for (index, image) in node.images.iter_mut().enumerate() {
                let hit = indexes.contains(&u64::try_from(index).unwrap_or(u64::MAX));
                if let fr_wire::content::ContentBlock::Known(
                    fr_wire::content::KnownContentBlock::Image { offloaded, .. },
                ) = image
                    && hit
                {
                    *offloaded = Some(fr_wire::content::TrueOnly);
                }
            }
        }
    }
}

/// Reassemble provider output from the message's exact embedded stream
/// (the dsh `_estimateProviderAssistant`).
///
/// # Errors
///
/// Returns the assembler's refusal for a malformed stream.
fn estimate_provider_assistant(
    stream: &[fr_wire::stream::AssistantStreamRecord],
) -> Result<u64, String> {
    let mut assembler = fr_llm::assembler::BlockAssembler::new();
    fr_llm::assistant_stream::assemble_assistant_stream(stream, &mut assembler);
    let blocks = assembler
        .blocks()
        .map_err(|error| format!("token meter: assistant stream assembly failed: {error}"))?;
    Ok(if blocks.is_empty() {
        0
    } else {
        estimate_content(&blocks).saturating_add(ROLE_OVERHEAD)
    })
}

/// The route pricing and file-text resolvers one measurement consults
/// (the dsh instance's soft `llm` seams, resolved by the shell over the
/// wire).
pub struct RouteResolvers<'a> {
    /// Prices the ordered image occurrences under one exact route,
    /// aligned with [`crate::route_pricing::collect_images`] over the
    /// priced surface; `None` keeps the fixed heuristic.
    pub image_prices: Box<ImagePriceResolver<'a>>,
    /// The exact request-time file handle text, when the mounted LLM
    /// service projects files.
    pub file_text: Option<Box<FileTextResolver<'a>>>,
}

impl std::fmt::Debug for RouteResolvers<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Resolver closures carry no diagnostic face; print presence.
        f.debug_struct("RouteResolvers")
            .field("image_prices", &"resolved per route")
            .field("file_text", &self.file_text.is_some())
            .finish()
    }
}

/// Measure current request pressure and surface through the durable
/// tail (the dsh `measure` body).
///
/// The effective envelope's routed provider/model selects the
/// request-image pricing every node is priced under: a route whose
/// adapter declares image pricing charges each retained image its
/// visual tokens plus its model-visible text, while other routes keep
/// the fixed heuristic. Provider usage is reused only when the latest
/// successful call's canonical request envelope matches `request_header`
/// and its total is no lower than that call's full route-priced anchor;
/// otherwise the complete envelope and surface are repriced. The anchor
/// includes all surface nodes immediately before the assistant message,
/// including inputs admitted after step/start.
///
/// `request_header` replaces the latest logged envelope for pressure
/// and node pricing; the node set always describes the current session
/// surface.
///
/// # Errors
///
/// Returns the fold's refusal texts.
pub fn measure(
    state: &ReplayState,
    request_header: Option<&EpochHeader>,
    resolvers: &RouteResolvers<'_>,
) -> Result<TokenMeasurement, String> {
    let header = request_header.map_or_else(
        || state.header.clone(),
        |header| Some(fr_session::canonical_header(header)),
    );
    let file_text = resolvers.file_text.as_deref();
    let surface = price_current(state, header.as_ref(), resolvers, file_text)?;
    let anchor = state.anchor.as_ref();

    let baseline;
    let surface_delta_tokens;
    if let Some(anchor) = anchor {
        if optional_header_equals(anchor.header.as_ref(), header.as_ref()) {
            // Matching headers share one route, so the anchored snapshot
            // reprices under the same pricing as the current surface and
            // the signed delta compares like with like.
            let anchor_surface_tokens =
                price_current_anchor(anchor, header.as_ref(), resolvers, file_text)?
                    .saturating_add(anchor.assistant_tokens);
            let estimated_anchor_tokens =
                estimate_tools_tokens(header.as_ref()).saturating_add(anchor_surface_tokens);
            // Signed heuristic deltas remain conservative only from an
            // anchor that is at least as large as the matching full
            // heuristic price.
            baseline = anchor
                .usage
                .filter(|usage| usage_tokens(usage) >= estimated_anchor_tokens)
                .map(|usage| MeasurementBaseline::Usage {
                    tokens: usage_tokens(&usage),
                    usage,
                })
                .unwrap_or(MeasurementBaseline::Estimated {
                    tokens: estimated_anchor_tokens,
                });
            surface_delta_tokens = i64::try_from(surface.surface_tokens)
                .unwrap_or(i64::MAX)
                .saturating_sub(i64::try_from(anchor_surface_tokens).unwrap_or(i64::MAX));
        } else {
            baseline = fallback_baseline(header.as_ref(), &surface);
            surface_delta_tokens = 0;
        }
    } else {
        baseline = fallback_baseline(header.as_ref(), &surface);
        surface_delta_tokens = 0;
    }

    let baseline_tokens = match &baseline {
        MeasurementBaseline::None => 0,
        MeasurementBaseline::Estimated { tokens } | MeasurementBaseline::Usage { tokens, .. } => {
            *tokens
        }
    };
    let total_tokens = u64::try_from(
        i64::try_from(baseline_tokens)
            .unwrap_or(i64::MAX)
            .saturating_add(surface_delta_tokens)
            .max(0),
    )
    .unwrap_or(u64::MAX);

    Ok(TokenMeasurement {
        log_revision: state.consumed_events,
        baseline,
        surface_delta_tokens,
        total_tokens,
        surface_tokens: surface.surface_tokens,
        nodes: surface.nodes,
    })
}

/// The no-anchor (or anchor-mismatch) baseline: the full envelope plus
/// surface price with zero delta (the dsh else-arms).
fn fallback_baseline(header: Option<&EpochHeader>, surface: &PricedSurface) -> MeasurementBaseline {
    if header.is_none() && surface.surface_tokens == 0 {
        return MeasurementBaseline::None;
    }
    MeasurementBaseline::Estimated {
        tokens: estimate_tools_tokens(header).saturating_add(surface.surface_tokens),
    }
}

/// Price the current surface under the route's resolvers.
fn price_current(
    state: &ReplayState,
    header: Option<&EpochHeader>,
    resolvers: &RouteResolvers<'_>,
    file_text: Option<&FileTextResolver>,
) -> Result<PricedSurface, String> {
    let pricing = route_prices(state.surface.as_slice(), header, resolvers);
    price_surface(&state.surface, pricing.as_deref(), file_text)
}

/// Price the anchored snapshot under the same route pricing.
fn price_current_anchor(
    anchor: &MeasurementAnchor,
    header: Option<&EpochHeader>,
    resolvers: &RouteResolvers<'_>,
    file_text: Option<&FileTextResolver>,
) -> Result<u64, String> {
    let pricing = route_prices(anchor.nodes.as_slice(), header, resolvers);
    let priced = price_surface(&anchor.nodes, pricing.as_deref(), file_text)?;
    Ok(priced.surface_tokens)
}

/// Resolve the route's per-occurrence prices over the mounted resolvers
/// (the dsh `_routeImagePricing` + `priceImages` pair): `Ok(None)`
/// keeps the fixed heuristic.
fn route_prices(
    nodes: &[MeterSurfaceNode],
    header: Option<&EpochHeader>,
    resolvers: &RouteResolvers<'_>,
) -> Option<Vec<ImageRequestPrice>> {
    let config = &header?.config;
    let images = crate::route_pricing::collect_images(nodes);
    if images.is_empty() {
        return None;
    }
    (resolvers.image_prices)(&config.provider, &config.model, &images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::estimate::BLOCK_OVERHEAD;
    use fr_wire::event::{AppendOp, SurfaceOp};
    use fr_wire::message::{KnownMessage, KnownMessageSource, Message, MessageSource};

    fn text_message(text: &str) -> Message {
        Message::Known(Box::new(KnownMessage::User {
            id: fr_wire::ids::MessageId::new("m"),
            content: vec![fr_wire::content::ContentBlock::Known(
                fr_wire::content::KnownContentBlock::Text {
                    text: text.to_owned(),
                },
            )],
            source: MessageSource::Known(KnownMessageSource::User {
                extra: fr_wire::content::RawObject::new(),
            }),
            extra: fr_wire::content::RawObject::new(),
        }))
    }

    fn append_event(seq: u64, turn: u64, step: u64, text: &str) -> SessionEvent {
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::AssistantMessage(fr_wire::event::AssistantMessageData {
                turn,
                step,
                message: text_message(text),
                stream: Vec::new(),
                usage: None,
                interrupted: None,
            }),
            ignorable: None,
            surface_op: Some(SurfaceOp::Append(AppendOp)),
            source_event_seqs: None,
        }
    }

    fn step_event(seq: u64, kind: &'static str, turn: u64, step: u64) -> SessionEvent {
        let data = if kind == "start" {
            SessionEventData::StepStart(fr_wire::event::StepBoundsData { turn, step })
        } else {
            SessionEventData::StepEnd(fr_wire::event::StepBoundsData { turn, step })
        };
        SessionEvent {
            seq: fr_wire::SessionSeq::new(seq).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data,
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        }
    }

    fn no_resolvers<'a>() -> RouteResolvers<'a> {
        RouteResolvers {
            image_prices: Box::new(|_, _, _| None),
            file_text: None,
        }
    }

    fn fold_all(state: &mut ReplayState, events: &[SessionEvent]) -> Result<(), String> {
        for event in events {
            fold_event(state, event)?;
            state.consumed_events += 1;
        }
        Ok(())
    }

    /// The empty surface before any request answers the none baseline
    /// with zero totals; appends price into the estimated baseline.
    #[test]
    fn none_baseline_then_estimated_growth() {
        let mut state = ReplayState::default();
        let measurement = measure(&state, None, &no_resolvers()).expect("empty measure");
        assert_eq!(measurement.baseline, MeasurementBaseline::None);
        assert_eq!(measurement.total_tokens, 0);
        assert_eq!(measurement.nodes.len(), 0);

        let events = [
            step_event(1, "start", 1, 0),
            append_event(2, 1, 0, "abcdefgh"),
            step_event(3, "end", 1, 0),
        ];
        fold_all(&mut state, &events).expect("fold");
        let measurement = measure(&state, None, &no_resolvers()).expect("measure");
        let expected = 8_u64.div_ceil(4) + BLOCK_OVERHEAD + ROLE_OVERHEAD;
        assert_eq!(measurement.nodes.len(), 1);
        assert_eq!(measurement.nodes[0].tokens, expected);
        assert_eq!(measurement.surface_tokens, expected);
        match measurement.baseline {
            MeasurementBaseline::Estimated { tokens } => assert_eq!(tokens, expected),
            other => panic!("estimated baseline expected: {other:?}"),
        }
        assert_eq!(measurement.total_tokens, expected);
        assert_eq!(measurement.log_revision, 3);
    }

    /// A settlement with usage under a known header arms the usage
    /// anchor; a matching later measure with surface growth reprices
    /// from the anchor delta.
    #[test]
    fn usage_anchor_tracks_surface_growth() {
        let mut state = ReplayState::default();
        let header = fr_wire::event::EpochHeader {
            config: fr_wire::call_config::CallConfig {
                provider: "p".to_owned(),
                model: "m".to_owned(),
                controls: fr_wire::call_config::CallControls::default(),
            },
            adapter_defaults: None,
            tools: None,
        };
        let header_event = SessionEvent {
            seq: fr_wire::SessionSeq::new(1).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::RequestHeader(fr_wire::event::RequestHeaderData {
                header,
                reason: fr_wire::event::RequestHeaderReason::Initial,
                starts_series: None,
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let settlement = {
            let mut event = append_event(2, 1, 0, "");
            if let SessionEventData::AssistantMessage(data) = &mut event.data {
                data.usage = Some(TokenUsage {
                    input_tokens: 500,
                    output_tokens: 10,
                    ..TokenUsage::default()
                });
            }
            event
        };
        let user_after = {
            let mut event = append_event(3, 1, 0, "user text here now");
            event.data = SessionEventData::UserMessage(text_message("user text here now"));
            event
        };
        fold_all(
            &mut state,
            &[
                header_event,
                step_event(4, "start", 1, 0),
                settlement,
                step_event(5, "end", 1, 0),
                user_after,
            ],
        )
        .expect("fold");
        // The pre-assistant surface (empty) anchors at usage 510; the
        // settlement node and the later user message reprice after it.
        let measurement = measure(&state, None, &no_resolvers()).expect("measure");
        let settlement_price = crate::estimate::estimate_message(&text_message(""));
        let user_price = crate::estimate::estimate_message(&text_message("user text here now"));
        assert_eq!(
            measurement.surface_tokens,
            settlement_price.saturating_add(user_price)
        );
        match measurement.baseline {
            MeasurementBaseline::Usage { tokens, usage } => {
                assert_eq!(tokens, 510);
                assert_eq!(usage.input_tokens, 500);
            }
            other => panic!("usage baseline expected: {other:?}"),
        }
        assert_eq!(
            measurement.total_tokens,
            510_u64
                .saturating_add(settlement_price)
                .saturating_add(user_price)
        );
        assert_eq!(
            measurement.surface_delta_tokens,
            i64::try_from(settlement_price)
                .unwrap_or(i64::MAX)
                .saturating_add(i64::try_from(user_price).unwrap_or(i64::MAX))
        );
    }

    /// Step pairing failures carry the dsh verbatim texts and leave the
    /// consumed offset semantics to the caller (the fold refuses
    /// without half-applying).
    #[test]
    fn step_pairing_fails_loud() {
        let error = fold_all(&mut ReplayState::default(), &[step_event(1, "end", 1, 0)])
            .expect_err("end without start");
        assert_eq!(
            error,
            "token meter: step/end at seq 1 has no matching step/start event"
        );

        let error = fold_all(
            &mut ReplayState::default(),
            &[step_event(1, "start", 1, 0), step_event(2, "start", 1, 1)],
        )
        .expect_err("nested start");
        assert_eq!(
            error,
            "token meter: step/start at seq 2 arrived before turn 1/step 0 ended"
        );

        let mut state = ReplayState::default();
        let error = fold_all(
            &mut state,
            &[step_event(1, "start", 1, 0), append_event(2, 2, 9, "x")],
        )
        .expect_err("message without matching start");
        assert_eq!(
            error,
            "token meter: assistant/message at seq 2 has no matching step/start event"
        );
    }

    /// The image/offload decision event marks the targeted occurrences;
    /// route pricing then answers through the resolver and replaces the
    /// structural prices.
    #[test]
    fn offload_marks_and_route_pricing_apply() {
        let mut state = ReplayState::default();
        let user_with_image = {
            let mut event = append_event(1, 1, 0, "");
            event.data =
                SessionEventData::UserMessage(Message::Known(Box::new(KnownMessage::User {
                    id: fr_wire::ids::MessageId::new("m"),
                    content: vec![fr_wire::content::ContentBlock::Known(
                        fr_wire::content::KnownContentBlock::Image {
                            attachment: fr_wire::content::ImageAttachmentRef {
                                attachment_id: fr_wire::ids::AttachmentId::new("a1"),
                                media_type: fr_wire::content::ImageMediaType::Png,
                                bytes: 64,
                                width: 8,
                                height: 8,
                                name: None,
                                original_dimensions: None,
                            },
                            offloaded: None,
                        },
                    )],
                    source: MessageSource::Known(KnownMessageSource::User {
                        extra: fr_wire::content::RawObject::new(),
                    }),
                    extra: fr_wire::content::RawObject::new(),
                })));
            event
        };
        let offload = SessionEvent {
            seq: fr_wire::SessionSeq::new(2).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::Unknown(fr_wire::event::UnknownEventData {
                type_name: "image/offload".to_owned(),
                data: serde_json::from_value(serde_json::json!({
                    "targets": [{"seq": 1, "imageIndexes": [0]}],
                }))
                .unwrap_or_else(|e| panic!("data: {e}")),
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        let header_event = SessionEvent {
            seq: fr_wire::SessionSeq::new(3).unwrap_or_else(|e| panic!("seq: {e}")),
            time: 1,
            data: SessionEventData::RequestHeader(fr_wire::event::RequestHeaderData {
                header: fr_wire::event::EpochHeader {
                    config: fr_wire::call_config::CallConfig {
                        provider: "vision".to_owned(),
                        model: "vl".to_owned(),
                        controls: fr_wire::call_config::CallControls::default(),
                    },
                    adapter_defaults: None,
                    tools: None,
                },
                reason: fr_wire::event::RequestHeaderReason::Initial,
                starts_series: None,
            }),
            ignorable: None,
            surface_op: None,
            source_event_seqs: None,
        };
        fold_all(&mut state, &[user_with_image, offload, header_event]).expect("fold");
        let images = crate::route_pricing::collect_images(&state.surface);
        assert_eq!(images.len(), 1);
        let offloaded = match &images[0] {
            fr_wire::content::ContentBlock::Known(fr_wire::content::KnownContentBlock::Image {
                offloaded,
                ..
            }) => offloaded.is_some(),
            _ => panic!("image block"),
        };
        assert!(offloaded);

        let resolvers = RouteResolvers {
            image_prices: Box::new(|_, _, images| {
                Some(
                    images
                        .iter()
                        .map(|_| fr_llm::adapter::ImageRequestPrice {
                            visual_tokens: 7,
                            text: "handle text".to_owned(),
                        })
                        .collect(),
                )
            }),
            file_text: None,
        };
        let measured = measure(&state, None, &resolvers).expect("measure");
        let heuristic = measured.nodes[0].heuristic_tokens;
        let structural = state.surface[0].image_structural_tokens;
        let text_price = crate::estimate::estimate_text_block("handle text");
        assert_eq!(
            measured.nodes[0].tokens,
            heuristic
                .saturating_sub(structural)
                .saturating_add(7 + text_price)
        );
    }
}
