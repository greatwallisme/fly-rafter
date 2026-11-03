//! The token-meter measurement vocabulary the compaction pipeline consumes
//! (the dsh `TokenMeasurement` family).
//!
//! The types mirror the token-meter service's wire payload exactly: the
//! runtime face of the seam is the dynamic-channel JSON (plugins never link
//! each other's runtime code), so the pipeline consumes this local mirror
//! and a wire-compatibility pin test (`tests/wire_shapes.rs`) guarantees the
//! mirror stays 1:1 with the producer's `to_wire` form.

use fr_wire::call_config::TokenUsage;
use serde::{Deserialize, Serialize};

/// The baseline from which a signed surface delta produces current pressure
/// (the token-meter service's `MeasurementBaseline`).
///
/// The wire face always carries `kind` + `tokens` (the none arm reports
/// `tokens: 0`), matching the producer's `to_wire`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MeasurementBaseline {
    /// No anchor exists yet: the empty surface before any request.
    #[serde(rename = "none")]
    None {
        /// Always wire `0` on the none arm.
        tokens: u64,
    },
    /// The signed delta reprices against a heuristic estimate.
    #[serde(rename = "estimated")]
    Estimated {
        /// Heuristic tokens of the anchored request shape.
        tokens: u64,
    },
    /// The signed delta reprices against a provider-reported usage sample.
    #[serde(rename = "usage")]
    Usage {
        /// The usage-anchored pressure.
        tokens: u64,
        /// The provider usage the anchor reported.
        usage: TokenUsage,
    },
}

/// One token-priced node in the current ordered session surface (the
/// token-meter service's `TokenSurfaceNode`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterSurfaceNode {
    /// Durable sequence number of the surface event.
    pub seq: u64,
    /// Request-pressure tokens for the exact message projected by this node
    /// under the measured route. Trigger, retention, and range selection
    /// all read this price.
    pub tokens: u64,
    /// Fixed-heuristic tokens for the same message, independent of any
    /// route. The shadow-price protocol prices replacements with this
    /// value.
    pub heuristic_tokens: u64,
}

/// Detached request-pressure and surface snapshot at one consumed log
/// revision (the token-meter service's `TokenMeasurement`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurfaceMeasurement {
    /// Number of durable events consumed; equal to the next unread event
    /// seq.
    pub log_revision: u64,
    /// Provider or heuristic anchor used for this measurement.
    pub baseline: MeasurementBaseline,
    /// Signed repricing of current surface content relative to the
    /// baseline anchor.
    pub surface_delta_tokens: i64,
    /// Non-negative current request-and-response pressure.
    pub total_tokens: u64,
    /// Total route-priced request tokens across the current surface; equals
    /// the sum of the node prices.
    pub surface_tokens: u64,
    /// Current surface nodes in positional head-to-tail order.
    pub nodes: Vec<MeterSurfaceNode>,
}
