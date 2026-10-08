//! Renderer-owned catalog contracts for cross-card segment fusion.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_node_engine::freeze::region::partition_regions;
use manifold_node_engine::freeze::segment::{concat_defs, def_is_segment_stateless};
use manifold_node_engine::persistence::PrimitiveRegistry;

#[cfg(test)]
mod tests {
    use crate::contracts::freeze::segment::*;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    fn card(json: &str) -> EffectGraphDef {
        serde_json::from_str(json).expect("parse card def")
    }

    const CARD_A: &str = r#"{
        "version": 1, "name": "cardA", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
            { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
            { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
            { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
        ]
    }"#;

    const CARD_B: &str = r#"{
        "version": 1, "name": "cardB", "nodes": [
            { "id": 0, "typeId": "system.source", "nodeId": "source" },
            { "id": 1, "typeId": "node.saturation", "nodeId": "sat" },
            { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
        ], "wires": [
            { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
            { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
        ]
    }"#;

    /// The headline structural claim: two pointwise cards concatenate into a
    /// def whose region finder produces ONE region spanning the seam — the
    /// seam round-trip is gone at the partition level.
    #[test]
    fn two_pointwise_cards_concat_into_one_region() {
        let a = card(CARD_A);
        let b = card(CARD_B);
        let seg = concat_defs(&[&a, &b]).expect("concat builds");

        // Boundaries: exactly one Source (card 0's) and one FinalOutput
        // (card 1's) survive.
        assert_eq!(
            seg.nodes.iter().filter(|n| n.type_id == "system.source").count(),
            1
        );
        assert_eq!(
            seg.nodes.iter().filter(|n| n.type_id == "system.final_output").count(),
            1
        );

        let regions = partition_regions(&seg, &registry());
        assert_eq!(regions.len(), 1, "the seam must not split the region");
        let member_ids: Vec<&str> = {
            let by_doc: std::collections::BTreeMap<u32, &str> = seg
                .nodes
                .iter()
                .map(|n| (n.id, n.node_id.as_str()))
                .collect();
            regions[0].members.iter().map(|m| by_doc[&m.doc_id]).collect()
        };
        assert_eq!(
            member_ids,
            vec!["c0.gain", "c0.contrast", "c1.sat"],
            "the region spans both cards' atoms in chain order"
        );
    }





    /// BUG-009: a card holding a StateStore-backed scalar primitive
    /// (`compressor_envelope`, as shipped in AutoGain) is NOT segment-stateless.
    /// It declares neither `state_capture_input_ports` nor `aliased_array_io`, so
    /// the old two-signal gate passed it — then, as a segment member, its
    /// `def_content_key: 0` made `harvest_state_from` skip it and any rebuild
    /// dropped its envelope (gain snapped to unity mid-show). The gate now also
    /// consults the truthful `requires().state_store` signal.
    #[test]
    fn state_store_scalar_card_is_not_segment_stateless() {
        let reg = registry();
        // Control: a pure-pointwise card stays eligible.
        assert!(
            def_is_segment_stateless(&card(CARD_A), &reg),
            "a pure pointwise card is segment-stateless",
        );

        // A card whose graph includes a compressor_envelope is not eligible.
        let stateful = card(
            r#"{
            "version": 1, "name": "autogain", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.compressor_envelope", "nodeId": "env" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#,
        );
        assert!(
            !def_is_segment_stateless(&stateful, &reg),
            "a StateStore-backed scalar node makes the card segment-ineligible (BUG-009)",
        );
    }
}
