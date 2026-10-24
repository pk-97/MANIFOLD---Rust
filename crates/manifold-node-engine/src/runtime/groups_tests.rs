use super::groups::{SplicedDef, splice_card_with_canonical_fallback};
use super::*;
use crate::scene::mesh_change::PreparedMeshRules;

fn def(map_type: &str) -> EffectGraphDef {
    serde_json::from_value(serde_json::json!({
        "version": 3, "name": "fallback-probe",
        "nodes": [
            {"id": 0, "nodeId": "source", "typeId": "system.source"},
            {"id": 1, "nodeId": "map", "handle": "map", "typeId": map_type},
            {"id": 2, "nodeId": "final_output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in"}
        ]
    }))
    .expect("probe def parses")
}

/// A fused splice that fails falls back to the canonical def and says so,
/// so the chain build binds the card through the canonical view: the
/// canonical binding reaches the spliced node, the fused one cannot.
#[test]
fn failed_fused_splice_reports_canonical_fallback() {
    let mut primitives = PrimitiveRegistry::with_builtin();
    crate::testkit::fusion_fixtures::register_fusion_test_nodes(&mut primitives);
    let fused = def("test.no_such_fused_kernel");
    let canonical = def("test.fusion_map");
    let mut graph = Graph::new();
    let source = graph.add_node(Box::new(crate::scene::boundary_nodes::Source::new()));
    let mut errors = Vec::new();
    let (splice, spliced) = splice_card_with_canonical_fallback(
        &mut graph,
        (source, "out"),
        &fused,
        &PreparedMeshRules::default(),
        &canonical,
        &PreparedMeshRules::default(),
        &primitives,
        Some((EffectId::new("fx"), PresetTypeId::new("Probe"))),
        false,
        &mut errors,
    )
    .expect("the canonical def splices");
    assert_eq!(spliced, SplicedDef::CanonicalFallback);
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ChainError::DivergentGraphFellBack { .. }))
    );

    let node_map: Vec<(NodeId, NodeInstanceId)> = splice
        .handles
        .iter()
        .filter_map(|(_, id)| graph.get_node(*id).map(|inst| (inst.node_id.clone(), *id)))
        .collect();
    assert!(
        node_map.iter().any(|(nid, _)| nid.as_str() == "map"),
        "the canonical binding's node is in the fallback graph",
    );
    assert!(
        !node_map
            .iter()
            .any(|(nid, _)| nid.as_str().starts_with("fused_region")),
        "a fused binding target does not exist in the fallback graph",
    );
}
