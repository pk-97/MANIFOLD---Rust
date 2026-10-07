//! Renderer-owned catalog contracts for freeze region partitioning.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_node_engine::freeze::classify::InputAccess;
use manifold_node_engine::freeze::region::*;
use manifold_node_engine::freeze::region::{build_region, final_reachable_nodes, wire_coincident_consumed};
use manifold_node_engine::persistence::PrimitiveRegistry;

fn registry() -> PrimitiveRegistry {
    PrimitiveRegistry::with_builtin()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D3 (BUG-114): a `BufferIndex`-tagged wire never unions and the array
    /// producer never becomes a region member — the array analogue of
    /// `gather_atom_folds_into_a_region` / `gather_wire_does_not_union` above.
    ///
    /// `draw_dots` carries a `Color` param, which independently keeps
    /// `classify_node` from ever admitting ANY atom into `eligible` (cut rule
    /// 4 — no non-scalar param may join a multi-node region; P5's scope, not
    /// this design's). This is the SAME reason six of wave2's seven
    /// shading-family atoms stay lone Boundaries despite being individually
    /// fusable (`wave2_color_param_atoms_stay_boundary_in_shipped_presets`),
    /// and it applies to every `draw_*` atom (all six carry a Color param) —
    /// so `partition_regions` itself can never exercise draw_dots as a region
    /// MEMBER until P5 lifts that gate. This test proves the D3 mechanism
    /// directly at the two layers that ARE reachable today: the wire-level
    /// gather contract (`input_port_access`/`wire_coincident_consumed`, which
    /// `partition_regions`' union filter reads regardless of the atom's
    /// overall eligibility) and `build_region` itself (called directly here,
    /// as `partition_regions` would once P5 makes draw_dots `eligible`).
    #[test]
    fn buffer_index_external_stays_external() {
        let json = r#"{
            "version": 1, "name": "dots-hud", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "test.fusion_map", "nodeId": "gain" },
                { "id": 2, "typeId": "node.blob_tracker", "nodeId": "blobs" },
                { "id": 3, "typeId": "node.draw_dots", "nodeId": "dots" },
                { "id": 4, "typeId": "node.saturation", "nodeId": "sat" },
                { "id": 5, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "blobs", "toNode": 3, "toPort": "detections" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();

        // draw_dots is on the codegen path (Pointwise, a real body, no
        // BoundaryReason) and, since P5 lifted Vec3/Vec4/Color params (D4
        // scope expansion, closing the P4a escalation), `classify_node` now
        // returns Eligible — the Color param no longer cuts it, and the
        // BufferIndex wire never did either. Pin both facts so a future
        // regression in either gate is caught here.
        let dots_node = def.nodes.iter().find(|n| n.id == 3).unwrap();
        let dots_prim = configured_construct(&registry(), dots_node).unwrap();
        assert_eq!(
            dots_prim.fusion_kind(),
            manifold_node_engine::freeze::classify::FusionKind::Pointwise
        );
        assert!(dots_prim.boundary_reason().is_none());
        assert_eq!(
            dots_prim.input_access(),
            &[InputAccess::Coincident, InputAccess::BufferIndex]
        );
        assert_eq!(
            classify_node(dots_node, &def, &registry()),
            NodeClass::Eligible,
            "P5 lifted the Color param — draw_dots is no longer cut by cut rule 4"
        );
        // Census verdict: draw_dots is Eligible now, so `classify_refusal`
        // has nothing to bucket for it (None) — the BufferIndex codegen gap
        // AND the Color-param gap are both closed.
        assert_eq!(census::classify_refusal(dots_node, &def, &registry()), None);

        // Wire-level contract: the array wire into `detections` is gather-
        // shaped (BufferIndex.is_gather()), so partition_regions' union
        // filter refuses it regardless of either endpoint's eligibility.
        let det_wire = def.wires.iter().find(|w| w.to_port == "detections").unwrap();
        assert!(
            !wire_coincident_consumed(&def, &registry(), det_wire),
            "a BufferIndex-consumed wire must never be a union candidate"
        );

        // build_region itself (the D3 mechanism): fed draw_dots directly,
        // bypassing the orthogonal Color-param eligibility filter — the real
        // region-assembly code path this design adds. Its array input must
        // resolve to an EXTERNAL (never a Member), naming blob_tracker.
        let final_reachable = final_reachable_nodes(&def);
        let region = build_region(&def, &registry(), &[3], &final_reachable, None)
            .expect("draw_dots assembles as a region shape on its own");
        assert_eq!(region.members.len(), 1);
        let dots = &region.members[0];
        assert_eq!(
            dots.input_access,
            vec![InputAccess::Coincident, InputAccess::BufferIndex]
        );
        assert_eq!(dots.inputs.len(), 2);
        let RegionInput::External(slot) = dots.inputs[1] else {
            panic!("detections must resolve to an external, not a member: {:?}", dots.inputs[1]);
        };
        assert_eq!(region.externals[slot].from_node, 2, "blob_tracker is the external producer");
        assert_eq!(region.externals[slot].from_port, "blobs");

        // P5's concrete proof (P4a's escalation, resolved): on the full graph
        // (not the isolated single-node `build_region` call above), draw_dots
        // now actually UNIONS with a texture neighbour (sat) into one real
        // region, closing the gap `wave2_color_param_atoms_stay_boundary_in_
        // shipped_presets` (P4a) pinned as still-open.
        let regions = partition_regions(&def, &registry());
        let dots_region = regions
            .iter()
            .find(|r| r.members.iter().any(|m| m.doc_id == 3))
            .expect("draw_dots forms a real region with a neighbor now that its Color param lifts");
        assert!(
            dots_region.members.iter().any(|m| m.doc_id == 4),
            "sat (the consumer) joins draw_dots' region"
        );
        // gain (node 1) stays OUTSIDE this region — not because of its own
        // Color/Vec3/Vec4 param (it has none), but because it fans out to
        // TWO consumers, one of them (blob_tracker) a boundary: a shared
        // producer with a non-fusable branch keeps its own texture rather
        // than being absorbed as a member (`shared_producer_is_not_absorbed`
        // is the same principle). Orthogonal to this test's point — the real
        // proof is that draw_dots + sat now form one 2-member region at all,
        // where before P5 draw_dots was Boundary and no region ever formed.
        assert!(
            !dots_region.members.iter().any(|m| m.doc_id == 1),
            "gain fans out to the boundary blob_tracker too, so it stays external"
        );
    }

    #[test]
    fn cut_remap_region_uses_map_capacity_when_source_is_shorter() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "source", "typeId": "node.gltf_mesh_source"},
                {"id": 1, "nodeId": "map", "typeId": "node.cut_mesh_bands"},
                {"id": 2, "nodeId": "remap", "typeId": "node.remap_mesh_cut"},
                {"id": 3, "nodeId": "rotate", "typeId": "node.rotate_3d"},
                {"id": 4, "nodeId": "object", "typeId": "node.scene_object"},
                {"id": 5, "nodeId": "render", "typeId": "node.render_scene"},
                {"id": 6, "nodeId": "output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "reference"},
                {"fromNode": 0, "fromPort": "vertices", "toNode": 2, "toPort": "in"},
                {"fromNode": 1, "fromPort": "map", "toNode": 2, "toPort": "map"},
                {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"},
                {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "vertices"},
                {"fromNode": 4, "fromPort": "object", "toNode": 5, "toPort": "object_0"},
                {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in"}
            ]
        })).unwrap();
        let regions = partition_regions(&def, &registry());
        let region = regions.iter().find(|region| region.members.iter().any(|m| m.doc_id == 2))
            .expect("remap and rotation must actually fuse");
        assert!(region.members.iter().any(|m| m.doc_id == 3));
        let capacities: Vec<_> = region.externals.iter().enumerate().map(|(i, external)|
            (format!("src_{i}"), if external.from_node == 1 { 30 } else { 3 })).collect();
        let refs: Vec<_> = capacities.iter().map(|(name, n)| (name.as_str(), *n)).collect();
        assert_eq!(region.output_capacity.as_ref().unwrap().eval(&refs), Some(30));
    }

    #[test]
    fn cut_reference_chain_keeps_cache_while_current_remap_can_fuse() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "source", "typeId": "node.gltf_mesh_source"},
                {"id": 1, "nodeId": "map", "typeId": "node.cut_mesh_bands"},
                {"id": 2, "nodeId": "reference", "typeId": "node.remap_mesh_cut"},
                {"id": 3, "nodeId": "current", "typeId": "node.remap_mesh_cut"},
                {"id": 4, "nodeId": "next_reference", "typeId": "node.remap_mesh_cut"},
                {"id": 5, "nodeId": "next_map", "typeId": "node.cut_mesh_cells"}
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "reference"},
                {"fromNode": 0, "fromPort": "vertices", "toNode": 2, "toPort": "in"},
                {"fromNode": 1, "fromPort": "map", "toNode": 2, "toPort": "map"},
                {"fromNode": 0, "fromPort": "vertices", "toNode": 3, "toPort": "in"},
                {"fromNode": 1, "fromPort": "map", "toNode": 3, "toPort": "map"},
                {"fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "in"},
                {"fromNode": 1, "fromPort": "map", "toNode": 4, "toPort": "map"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "reference"}
            ]
        }))
        .expect("cut reference fixture");
        let registry = registry();
        for id in [2, 4] {
            assert!(matches!(
                classify_node(&def.nodes[id], &def, &registry),
                NodeClass::Boundary
            ));
        }
        assert_eq!(
            classify_node(&def.nodes[3], &def, &registry),
            NodeClass::Eligible
        );
    }

    /// D4/P6 regression guard (found + fixed by the Glitch real-preset proof):
    /// a MULTI-output node's two ports can each union independently into
    /// otherwise-unrelated branches — one branch ends in a node whose output
    /// GATHER-feeds the other branch's node. Neither branch unions with the
    /// other directly (the gather wire is correctly excluded from union
    /// candidates), but the multi-output producer bridges them into ONE
    /// component via two separate coincident wires. `build_region` would
    /// then find the gather wire's endpoints BOTH inside that one merged
    /// component and bail the WHOLE thing to unfused — costing every member,
    /// not just the gather pair. The gather-bridge guard in `partition_regions`
    /// must keep the two components separate instead, so each still fuses
    /// on its own and the two connect via the SAME cross-region gather the
    /// multi-region model already relies on.
    ///
    /// Topology: `cells` (Source, 2 texture outputs) → `out` feeds `invert`
    /// feeds `remap.uv_field` (branch A); `cells` → `cell_id` feeds
    /// `hash.field` (branch B). `remap`'s output GATHER-feeds `rgb_split.in`;
    /// `hash`'s output feeds `rgb_split.velocity` (Coincident) — so without
    /// the guard, `cells` bridges A and B into one component that contains
    /// both `remap` and `rgb_split`, the gather pair.
    #[test]
    fn multi_output_producer_never_bridges_a_gather_pair_into_one_region() {
        let json = r#"{
            "version": 1, "name": "gather_bridge", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.voronoi_2d", "nodeId": "cells" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.remap", "nodeId": "remap" },
                { "id": 4, "typeId": "node.hash_field_by_seed", "nodeId": "hash" },
                { "id": 5, "typeId": "node.rgb_split", "nodeId": "split" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "uv_field" },
                { "fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "source" },
                { "fromNode": 1, "fromPort": "cell_id", "toNode": 4, "toPort": "field" },
                { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "velocity" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let mut regions = partition_regions(&def, &registry());
        // Two separate regions (branch A: cells+invert+remap; branch B: hash
        // alone, or folded with whichever side the finder groups it) — NEVER
        // one region containing both `remap` (3) and `rgb_split` (5), which
        // would mean the gather pair got bridged into one component.
        for r in &regions {
            let ids: Vec<u32> = r.members.iter().map(|m| m.doc_id).collect();
            assert!(
                !(ids.contains(&3) && ids.contains(&5)),
                "remap and rgb_split (a gather pair) must never share a region: {ids:?}"
            );
        }
        regions.sort_by_key(|r| r.members[0].doc_id);
        assert!(
            regions.iter().any(|r| r.members.iter().any(|m| m.doc_id == 1)
                && r.members.iter().any(|m| m.doc_id == 3)),
            "cells must still fuse with its OWN branch (invert + remap)"
        );
    }

    /// A specialization-constant atom now FUSES: classify substitutes the
    /// declared tokens (`QUALITY_LEVEL` / `WEIGHTING_MODE`) with the def's
    /// static param values before the naga parse gate, so
    /// `gaussian_blur_variable_width` stops being a permanent boundary. Here
    /// the upstream invert is a stranded single absorbed into the blur's `in`
    /// fetch; the `width` input gathers the source as a real external; the
    /// downstream invert threads the blur's register. One region.
    #[test]
    fn specialization_atom_fuses_with_substituted_tokens() {
        let json = r#"{
            "version": 1, "name": "spec", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.invert", "nodeId": "inv_a" },
                { "id": 2, "typeId": "node.variable_blur", "nodeId": "blur" },
                { "id": 3, "typeId": "node.invert", "nodeId": "inv_b" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "width" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "the variable-width blur fuses");
        let r = &regions[0];
        assert_eq!(
            r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![2, 3],
            "blur + downstream invert"
        );
        assert_eq!(r.virtual_chains.len(), 1, "the upstream invert is absorbed");
        assert_eq!(r.virtual_chains[0].members[0].doc_id, 1);
        assert_eq!(r.externals.len(), 1, "the source backs both the chain and width");
        assert_eq!(r.outputs, vec![(3, "out".to_string())]);
    }

    /// Stencil tier — a STRANDED SINGLE producer (a pointwise atom whose only
    /// consumer is a stencil blur's gather input) is absorbed into the blur's
    /// fetch as a virtual chain: one region, member = the blur, the gain
    /// recomputed per tap corner, the source as the chain's external. Without
    /// absorption both nodes are lone components and nothing fuses.
    #[test]
    fn stranded_single_absorbs_into_blur_fetch() {
        let json = r#"{
            "version": 1, "name": "stencil", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.gaussian_blur", "nodeId": "blur" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "blur + absorbed gain form one region");
        let r = &regions[0];
        assert_eq!(r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(r.virtual_chains.len(), 1, "gain absorbed as a virtual chain");
        let chain = &r.virtual_chains[0];
        assert_eq!(chain.consumer, 2);
        assert_eq!(chain.input_index, 0);
        assert_eq!(chain.output, 1);
        assert_eq!(chain.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(r.externals.len(), 1, "the source backs the chain");
        assert_eq!(r.externals[0].from_node, 0);
        assert_eq!(chain.members[0].inputs, vec![RegionInput::External(0)]);
        let blur = &r.members[0];
        assert_eq!(blur.inputs, vec![RegionInput::Virtual(0)], "the blur reads the chain");
        assert_eq!(r.outputs, vec![(2, "out".to_string())]);
    }

    /// Checkpoint (wgsl_compute fusion contract): a FRAGMENT-form `node.wgsl_compute`
    /// is a first-class fusable atom — an atom → fragment → atom chain partitions
    /// into ONE region holding all three. The fragment reports `Pointwise` + a
    /// `wgsl_body` only because `configured_construct` applies its `wgslSource`
    /// before the classifier reads it; a bare construct sees the opaque default
    /// kernel (Boundary) and the chain would split into three singletons. Note the
    /// fragment's output port is `dst` — the name the standalone codegen gives the
    /// single storage-texture output it synthesizes.
    #[test]
    fn wgsl_compute_fragment_fuses_with_atoms() {
        use manifold_node_engine::freeze::markers::Marker;
        // Same placeholder-substitution convention as the proof.rs sibling of this
        // fixture — the `@fusion: pointwise` marker is routed through `Marker::emit`
        // rather than hand-typed, so this test stays off the single-sourced grammar's
        // negative gate.
        let json = r#"{
            "version": 1, "name": "frag", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain",
                  "params": { "gain": { "type": "Float", "value": 1.2 } } },
                { "id": 2, "typeId": "node.wgsl_compute", "nodeId": "frag",
                  "wgslSource": "FUSION_MARKER\n// @in: src\n// @param: scale = 0.75 [0, 2]\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scale: f32) -> vec4<f32> {\n    return vec4<f32>(c.rgb * scale, c.a);\n}\n",
                  "params": { "scale": { "type": "Float", "value": 0.75 } } },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "src" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#
        .replacen("FUSION_MARKER", &Marker::Fusion { kind: "pointwise".to_string() }.emit(), 1);
        let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "gain + fragment + invert form one region");
        assert_eq!(
            regions[0].members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the fragment-form wgsl_compute fuses between the two atoms"
        );
        assert!(regions[0].virtual_chains.is_empty());
    }

    /// BUG-x72p (scene-mirror-blocked-gather-input-fusion): a `BufferGather`
    /// atom ADMITS into buffer regions — the gathered wire stays external, the
    /// node fuses. scatter → neighbor_smooth (gathered `in`) and scatter →
    /// blend_copies.a (coincident) read the SAME producer port, so the finder
    /// dedupes them into ONE external slot read both ways; smooth joins the
    /// region through its coincident consumer (blend_copies.b).
    #[test]
    fn buffer_gather_atom_fuses_with_gathered_wire_external() {
        let json = r#"{
            "version": 1,
            "nodes": [
                { "id": 0, "typeId": "node.scatter_on_mesh", "nodeId": "scatter" },
                { "id": 1, "typeId": "node.neighbor_smooth", "nodeId": "smooth" },
                { "id": 2, "typeId": "node.blend_copies", "nodeId": "blend" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "instances", "toNode": 1, "toPort": "in" },
                { "fromNode": 0, "fromPort": "instances", "toNode": 2, "toPort": "a" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "smooth + blend form one region");
        let r = &regions[0];
        assert_eq!(
            r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 2],
            "the BufferGather atom joined through its coincident consumer"
        );
        assert_eq!(r.externals.len(), 1, "scatter.instances deduped to one slot");
        assert_eq!(r.externals[0].from_node, 0);
        assert_eq!(r.externals[0].from_port, "instances");
        let smooth = r.members.iter().find(|m| m.doc_id == 1).unwrap();
        assert_eq!(smooth.inputs, vec![RegionInput::External(0)]);
        assert_eq!(smooth.input_access, vec![InputAccess::BufferGather]);
        let blend = r.members.iter().find(|m| m.doc_id == 2).unwrap();
        assert_eq!(
            blend.inputs,
            vec![RegionInput::External(0), RegionInput::Member(1)],
            "blend.a reads the same slot coincidently, blend.b threads smooth"
        );
        assert_eq!(
            blend.input_access,
            vec![InputAccess::Coincident, InputAccess::Coincident]
        );
        assert_eq!(r.outputs, vec![(2, "out".to_string())]);
    }

    /// BUG-x72p companion: the gather-bridge guard covers ARRAY wires too. A
    /// gathered producer must never land in the same region as its consumer,
    /// even when the merge is convexity-clean (`gather_pairs` is what refuses
    /// it). Ids matter: smooth_b (id 1, the gather consumer) merges with blend
    /// first — convex there — and smooth_a's later merge (id 2, the gathered
    /// producer feeding blend.a) would collapse the gather pair (2, 1) into
    /// one region without the guard, which `build_region` would then refuse
    /// whole ("gather input wired from a member"). With the guard, smooth_b +
    /// blend fuse and read smooth_a's output as an external — one slot,
    /// read both ways (gathered by smooth_b, coincident by blend.a).
    #[test]
    fn buffer_gather_bridge_guard_keeps_producer_out_of_consumer_region() {
        let json = r#"{
            "version": 1,
            "nodes": [
                { "id": 0, "typeId": "node.scatter_on_mesh", "nodeId": "scatter" },
                { "id": 1, "typeId": "node.neighbor_smooth", "nodeId": "smooth_b" },
                { "id": 2, "typeId": "node.neighbor_smooth", "nodeId": "smooth_a" },
                { "id": 3, "typeId": "node.blend_copies", "nodeId": "blend" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "instances", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "a" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "b" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert_eq!(regions.len(), 1, "smooth_b + blend fuse");
        let r = &regions[0];
        assert_eq!(
            r.members.iter().map(|m| m.doc_id).collect::<Vec<_>>(),
            vec![1, 3],
            "smooth_a is kept out of its gathered consumer's region"
        );
        assert_eq!(r.externals.len(), 1, "smooth_a.out deduped to one slot");
        assert_eq!(r.externals[0].from_node, 2);
        let smooth_b = r.members.iter().find(|m| m.doc_id == 1).unwrap();
        assert_eq!(smooth_b.input_access, vec![InputAccess::BufferGather]);
        let blend = r.members.iter().find(|m| m.doc_id == 3).unwrap();
        assert_eq!(
            blend.inputs,
            vec![RegionInput::External(0), RegionInput::Member(1)],
            "blend.a reads the same slot coincidently, blend.b threads smooth_b"
        );
    }

    /// BUG-orm4 widened gate: reflect_array DECLARES its 2x output capacity,
    /// so the region it heads would widen the dispatch count to
    /// `2 x arrayLength(&src_0)`. blend.a reads the SAME producer port the
    /// reflect gathers, as a COINCIDENT external — the pre-read
    /// `src_0[idx]` would run off the input's end at the widened count. The
    /// widened gate refuses the region (renders unfused, always correct).
    /// The sound shape — the multiplier's output consumed as a register —
    /// is pinned in reflect_array.rs's
    /// `reflect_array_enters_a_fused_region_with_widened_count`.
    #[test]
    fn buffer_gather_widened_region_refuses_coincident_array_external() {
        let json = r#"{
            "version": 1,
            "nodes": [
                { "id": 0, "typeId": "node.scatter_on_mesh", "nodeId": "scatter" },
                { "id": 1, "typeId": "node.reflect_array", "nodeId": "reflect" },
                { "id": 2, "typeId": "node.blend_copies", "nodeId": "blend" },
                { "id": 3, "typeId": "system.final_output", "nodeId": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "instances", "toNode": 1, "toPort": "in" },
                { "fromNode": 0, "fromPort": "instances", "toNode": 2, "toPort": "a" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert!(
            regions.is_empty(),
            "the widened gates refuse the coincident array external; \
             singletons are below MIN_REGION_LEN — nothing fuses, render \
             unfused (always correct)"
        );
    }

    /// BUG-orm4 widened gate, companion: a NON-multiplier gathered member is
    /// only bounds-safe at its OWN dispatch count — neighbor_smooth reads
    /// `buf_in[idx ± 1]` and clamps by the grid geometry, which assumes
    /// `idx < arrayLength`. Inside a region whose count a MultipleOf member
    /// (reflect_array, 2x) widened, that assumption breaks. Here scatter
    /// feeds BOTH gathers and blend merges the two chains' registers, so
    /// convexity would put reflect + smooth + blend in ONE region — the
    /// widened gate refuses it (renders unfused, always correct).
    #[test]
    fn buffer_gather_widened_region_refuses_non_multiplier_gather() {
        let json = r#"{
            "version": 1,
            "nodes": [
                { "id": 0, "typeId": "node.scatter_on_mesh", "nodeId": "scatter" },
                { "id": 1, "typeId": "node.reflect_array", "nodeId": "reflect" },
                { "id": 2, "typeId": "node.neighbor_smooth", "nodeId": "smooth" },
                { "id": 3, "typeId": "node.blend_copies", "nodeId": "blend" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "instances", "toNode": 1, "toPort": "in" },
                { "fromNode": 0, "fromPort": "instances", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "a" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "b" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let regions = partition_regions(&def, &registry());
        assert!(
            regions.is_empty(),
            "the widened gate refuses smooth's non-multiplier gather; \
             singletons are below MIN_REGION_LEN — nothing fuses, render \
             unfused (always correct)"
        );
    }

    /// BUG-2efy (capacity probe admits an output that follows slot 0): a
    /// member whose output follows one input, declared MinInputs, never fuses.
    /// test.follow_first's output follows `a`, the region's first external;
    /// fused, the count would be the min over a and b. One ascending probe
    /// order agrees with the black box by accident; the descending order
    /// catches it. The divide's gathered divisor puts the region through the
    /// probe.
    #[test]
    fn output_following_one_input_is_refused_under_min_inputs() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "a", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 2049}}},
                {"id": 1, "nodeId": "b", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 2048}}},
                {"id": 2, "nodeId": "follow", "typeId": "test.follow_first"},
                {"id": 3, "nodeId": "divisor", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 1}}},
                {"id": 4, "nodeId": "divide", "typeId": "node.divide_by_value"},
                {"id": 5, "nodeId": "sink", "typeId": "test.value_sink"},
                {"id": 6, "nodeId": "output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "a"},
                {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "b"},
                {"fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "values"},
                {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "divisor"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "values"},
                {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in"}
            ]
        }))
        .expect("selector fixture");
        let mut registry = registry();
        manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes(&mut registry);
        let regions = partition_regions(&def, &registry);
        let fused: Vec<Vec<u32>> = regions.iter().map(|r| r.members.iter().map(|m| m.doc_id).collect()).collect();
        assert!(
            fused.iter().all(|members| !members.contains(&2)),
            "follow_first's output follows a alone, so it must not fuse as MinInputs: {fused:?}"
        );
        // Declared honestly (FromInput), the same shape fuses: the refusal
        // above is the probe's, not a gate the chain trips anyway.
        assert_eq!(partition_regions(&honest_chain(), &registry).len(), 1, "a solid clamp into the divide fuses");
    }

    /// The same chain with an honest producer: node.clamp_liquid_to_solids (its
    /// output follows its coincident level set, declared FromInput) into the
    /// divide.
    fn honest_chain() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [
                {"id": 0, "nodeId": "solid", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 64}}},
                {"id": 1, "nodeId": "levelset", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 512}}},
                {"id": 2, "nodeId": "clamp", "typeId": "node.clamp_liquid_to_solids"},
                {"id": 3, "nodeId": "divisor", "typeId": "test.value_source", "params": {"max_capacity": {"type": "Int", "value": 1}}},
                {"id": 4, "nodeId": "divide", "typeId": "node.divide_by_value"},
                {"id": 5, "nodeId": "sink", "typeId": "test.value_sink"},
                {"id": 6, "nodeId": "output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "levelset"},
                {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "solid"},
                {"fromNode": 2, "fromPort": "clamped", "toNode": 4, "toPort": "values"},
                {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "divisor"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "values"},
                {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in"}
            ]
        }))
        .expect("honest fixture")
    }
}
