//! Wire-shape compatibility between the compaction pipeline's local
//! measurement mirror and the token-meter service's producer face (the
//! dynamic-channel seam contract).
//!
//! The pins ride JSON literals rather than the producer rlib: one test
//! binary may not link two plugin rlibs (the batch-2a
//! `DOUGONG_PLUGIN_EXPORTS` link rule — the compaction artifact carries
//! its own export now, so the 17e producer-linked pins cannot link).
//! The literal shapes pin the producer's `to_wire` output byte-for-byte;
//! fr-token-meter's own crate pins the same output against its `to_wire`
//! implementation, so the two-sided agreement stays honest.

use fr_compaction::meter::{MeasurementBaseline, MeterSurfaceNode, SurfaceMeasurement};

/// The producer measurement's wire form (fr-token-meter
/// `types::TokenMeasurement::to_wire` output for the usage-anchored
/// fixture; the literal pins the byte shape).
fn producer_wire() -> serde_json::Value {
    serde_json::json!({
        "logRevision": 12,
        "baseline": {
            "kind": "usage",
            "tokens": 900,
            "usage": {
                "inputTokens": 800,
                "outputTokens": 100,
                "totalTokens": 900,
            },
        },
        "surfaceDeltaTokens": -40,
        "totalTokens": 860,
        "surfaceTokens": 900,
        "nodes": [
            { "seq": 3, "tokens": 400, "heuristicTokens": 410 },
            { "seq": 7, "tokens": 500, "heuristicTokens": 490 },
        ],
    })
}

#[test]
fn measurement_mirror_parses_the_producer_wire_shape() {
    let mirrored: SurfaceMeasurement =
        serde_json::from_value(producer_wire()).expect("mirror parse");
    assert_eq!(mirrored.log_revision, 12);
    assert_eq!(mirrored.total_tokens, 860);
    assert_eq!(mirrored.nodes.len(), 2);
    assert_eq!(mirrored.nodes[0].seq, 3);
    assert_eq!(mirrored.nodes[0].heuristic_tokens, 410);
    let MeasurementBaseline::Usage { tokens, .. } = &mirrored.baseline else {
        panic!("the usage baseline survives the seam");
    };
    assert_eq!(*tokens, 900);
}

#[test]
fn measurement_mirror_round_trips_all_three_baselines() {
    let wire = producer_wire();
    let mirrored: SurfaceMeasurement = serde_json::from_value(wire.clone()).expect("parse");
    let back = serde_json::to_value(&mirrored).expect("re-emit");
    assert_eq!(back, wire, "the mirror re-emits the producer's wire bytes");

    let estimated = serde_json::json!({
        "logRevision": 12,
        "baseline": { "kind": "estimated", "tokens": 640 },
        "surfaceDeltaTokens": -40,
        "totalTokens": 860,
        "surfaceTokens": 900,
        "nodes": [],
    });
    let mirrored: SurfaceMeasurement = serde_json::from_value(estimated).expect("estimated parse");
    assert_eq!(
        mirrored.baseline,
        MeasurementBaseline::Estimated { tokens: 640 }
    );

    let none = serde_json::json!({
        "logRevision": 12,
        "baseline": { "kind": "none", "tokens": 0 },
        "surfaceDeltaTokens": 0,
        "totalTokens": 0,
        "surfaceTokens": 0,
        "nodes": [],
    });
    let mirrored: SurfaceMeasurement = serde_json::from_value(none).expect("none parse");
    assert_eq!(mirrored.baseline, MeasurementBaseline::None { tokens: 0 });
}

#[test]
fn meter_surface_node_mirror_matches_field_for_field() {
    let wire = producer_wire();
    let expected = MeterSurfaceNode {
        seq: 3,
        tokens: 400,
        heuristic_tokens: 410,
    };
    let mirrored: MeterSurfaceNode =
        serde_json::from_value(wire["nodes"][0].clone()).expect("node parse");
    assert_eq!(mirrored, expected);
}
