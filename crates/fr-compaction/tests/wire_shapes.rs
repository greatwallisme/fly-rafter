//! Wire-shape compatibility between the compaction pipeline's local
//! measurement mirror and the token-meter service's producer face (the
//! dynamic-channel seam contract).

use fr_compaction::meter::{MeasurementBaseline, MeterSurfaceNode, SurfaceMeasurement};
use fr_token_meter::types::{
    MeasurementBaseline as ProducerBaseline, TokenMeasurement as ProducerMeasurement,
    TokenSurfaceNode as ProducerNode,
};

fn producer_measurement() -> ProducerMeasurement {
    ProducerMeasurement {
        log_revision: 12,
        baseline: ProducerBaseline::Usage {
            tokens: 900,
            usage: fr_wire::call_config::TokenUsage {
                input_tokens: 800,
                output_tokens: 100,
                total_tokens: Some(900),
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
        },
        surface_delta_tokens: -40,
        total_tokens: 860,
        surface_tokens: 900,
        nodes: vec![
            ProducerNode {
                seq: 3,
                tokens: 400,
                heuristic_tokens: 410,
            },
            ProducerNode {
                seq: 7,
                tokens: 500,
                heuristic_tokens: 490,
            },
        ],
    }
}

#[test]
fn measurement_mirror_parses_the_producer_wire_shape() {
    let wire = producer_measurement().to_wire();
    let mirrored: SurfaceMeasurement = serde_json::from_value(wire).expect("mirror parse");
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
    let wire = producer_measurement().to_wire();
    let mirrored: SurfaceMeasurement = serde_json::from_value(wire.clone()).expect("parse");
    let back = serde_json::to_value(&mirrored).expect("re-emit");
    assert_eq!(back, wire, "the mirror re-emits the producer's wire bytes");

    let estimated = ProducerMeasurement {
        baseline: ProducerBaseline::Estimated { tokens: 640 },
        ..producer_measurement()
    };
    let wire = estimated.to_wire();
    let mirrored: SurfaceMeasurement = serde_json::from_value(wire).expect("estimated parse");
    assert_eq!(
        mirrored.baseline,
        MeasurementBaseline::Estimated { tokens: 640 }
    );

    let none = ProducerMeasurement {
        baseline: ProducerBaseline::None,
        ..producer_measurement()
    };
    let wire = none.to_wire();
    assert_eq!(
        wire["baseline"],
        serde_json::json!({"kind": "none", "tokens": 0})
    );
    let mirrored: SurfaceMeasurement = serde_json::from_value(wire).expect("none parse");
    assert_eq!(mirrored.baseline, MeasurementBaseline::None { tokens: 0 });
}

#[test]
fn meter_surface_node_mirror_matches_field_for_field() {
    let wire = producer_measurement().to_wire();
    let expected = MeterSurfaceNode {
        seq: 3,
        tokens: 400,
        heuristic_tokens: 410,
    };
    let mirrored: MeterSurfaceNode =
        serde_json::from_value(wire["nodes"][0].clone()).expect("node parse");
    assert_eq!(mirrored, expected);
}
