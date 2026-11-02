//! Public measurement vocabulary for replay token metering (dsh
//! `token-meter/types.ts` verbatim shapes).

#![forbid(unsafe_code)]

use fr_wire::call_config::TokenUsage;

/// The baseline from which a signed surface delta produces current
/// pressure (dsh `TokenMeasurementBaseline`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeasurementBaseline {
    /// No anchor exists yet: the empty surface before any request.
    None,
    /// The signed delta reprices against a heuristic estimate.
    Estimated {
        /// Heuristic tokens of the anchored request shape.
        tokens: u64,
    },
    /// The signed delta reprices against a provider-reported usage
    /// sample.
    Usage {
        /// The usage-anchored pressure.
        tokens: u64,
        /// The provider usage the anchor reported.
        usage: TokenUsage,
    },
}

/// One token-priced node in the current ordered session surface (dsh
/// `TokenSurfaceNode`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSurfaceNode {
    /// Durable sequence number of the surface event.
    pub seq: u64,
    /// Request-pressure tokens for the exact message projected by this
    /// node under the measured route: image occurrences carry the
    /// route's declared visual price when the routed adapter declares
    /// one, and the fixed heuristic otherwise. Trigger, retention, and
    /// range selection all read this price.
    pub tokens: u64,
    /// Fixed-heuristic tokens for the same message, independent of any
    /// route. The shadow-price protocol prices replacements with this
    /// value so the O(1) projection fold stays in agreement with its
    /// own appends.
    pub heuristic_tokens: u64,
}

/// Detached request-pressure and surface snapshot at one consumed log
/// revision (dsh `TokenMeasurement`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenMeasurement {
    /// Number of durable events consumed; equal to the next unread
    /// event seq.
    pub log_revision: u64,
    /// Provider or heuristic anchor used for this measurement.
    pub baseline: MeasurementBaseline,
    /// Signed repricing of current surface content relative to the
    /// baseline anchor.
    pub surface_delta_tokens: i64,
    /// Non-negative current request-and-response pressure.
    pub total_tokens: u64,
    /// Total route-priced request tokens across the current surface;
    /// equals the sum of the node prices.
    pub surface_tokens: u64,
    /// Current surface nodes in positional head-to-tail order.
    pub nodes: Vec<TokenSurfaceNode>,
}

impl TokenMeasurement {
    /// The wire form (camelCase keys, dsh JSON shape).
    #[must_use]
    pub fn to_wire(&self) -> serde_json::Value {
        let baseline = match &self.baseline {
            MeasurementBaseline::None => serde_json::json!({ "kind": "none", "tokens": 0 }),
            MeasurementBaseline::Estimated { tokens } => {
                serde_json::json!({ "kind": "estimated", "tokens": tokens })
            }
            MeasurementBaseline::Usage { tokens, usage } => {
                serde_json::json!({ "kind": "usage", "tokens": tokens, "usage": usage })
            }
        };
        serde_json::json!({
            "logRevision": self.log_revision,
            "baseline": baseline,
            "surfaceDeltaTokens": self.surface_delta_tokens,
            "totalTokens": self.total_tokens,
            "surfaceTokens": self.surface_tokens,
            "nodes": self.nodes.iter().map(|node| serde_json::json!({
                "seq": node.seq,
                "tokens": node.tokens,
                "heuristicTokens": node.heuristic_tokens,
            })).collect::<Vec<_>>(),
        })
    }
}
