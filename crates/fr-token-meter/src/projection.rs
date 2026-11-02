//! Pure client-safe token-projection vocabulary (dsh
//! `token-meter/projection.ts` 1:1): the wire shapes the projection
//! units publish through the relay.

#![forbid(unsafe_code)]

/// Disjoint provider usage buckets of one complete session log (dsh
/// `TokenUsageProjection`): the `tokenUsage` unit's wire view.
pub use crate::usage_projection::TokenUsageBuckets as TokenUsageProjection;

/// The context-pressure wire view (dsh `ContextPressureProjection`):
/// newest request pressure paired with the newest known route capacity,
/// plus the surface-delta projection.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPressureProjection {
    /// Provider-reported prompt size of the most recent request:
    /// uncached input plus cache reads and writes. Response output is
    /// excluded, so this does not grow as the current turn streams.
    /// Absent until a provider reports usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_tokens: Option<u64>,
    /// What the NEXT request's prompt would cost: [`Self::pressure_tokens`]
    /// plus the heuristic repricing of everything the surface gained or
    /// lost since that sample. Absent until a provider reports usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projected_tokens: Option<u64>,
    /// Newest recorded route capacity; absent when no adapter
    /// advertised one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// Heuristic composition of the next request's context (dsh
/// `ContextBreakdownProjection`): what the prompt is made of, not what
/// it costs.
pub use crate::breakdown_projection::Breakdown as ContextBreakdownProjection;

impl ContextPressureProjection {
    /// The pressure unit's wire view (the dsh `wire.view`): presence
    /// decides each field, and `projectedTokens` publishes only from a
    /// sampled anchor.
    #[must_use]
    pub fn view(state: &crate::usage_projection::ContextPressureState) -> Self {
        let projected_tokens =
            state
                .pressure_tokens
                .zip(state.sampled_surface_tokens)
                .map(|(pressure, sampled)| {
                    u64::try_from(
                        i64::try_from(pressure)
                            .unwrap_or(i64::MAX)
                            .saturating_add(i64::try_from(state.surface_tokens).unwrap_or(i64::MAX))
                            .saturating_sub(i64::try_from(sampled).unwrap_or(i64::MAX))
                            .max(0),
                    )
                    .unwrap_or(u64::MAX)
                });
        Self {
            pressure_tokens: state.pressure_tokens,
            projected_tokens,
            context_window: state.context_window,
        }
    }
}
