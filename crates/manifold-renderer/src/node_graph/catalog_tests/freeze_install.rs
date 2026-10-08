use manifold_node_engine::freeze::install::*;


use ahash::AHashSet;
use manifold_core::effect_graph_def::EffectGraphDef;

use manifold_node_engine::persistence::PrimitiveRegistry;

fn registry() -> PrimitiveRegistry { PrimitiveRegistry::with_builtin() }


    fn colorgrade_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/effect-presets/ColorGrade.json"
        ))
        .expect("read ColorGrade.json");
        serde_json::from_str(&json).expect("parse ColorGrade.json")
    }

    /// The whole ColorGrade card (7 atoms, one region) collapses to ONE
    /// `node.wgsl_compute` node between the retained boundaries, wired
    /// source → fused.src_0 → final_output. The retarget maps each inner
    /// (node_id, param) to its region's fused node + `n{i}_{param}` field — the
    /// load-bearing routing for the binding rewrite.
    #[test]
    fn colorgrade_fuses_to_single_wgsl_node() {
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");

        // 3 nodes: source, fused, final_output. 2 wires.
        assert_eq!(fused.def.nodes.len(), 3, "boundaries + one fused node");
        let wgsl_nodes: Vec<_> = fused
            .def
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .collect();
        assert_eq!(wgsl_nodes.len(), 1, "exactly one fused node");
        assert!(wgsl_nodes[0].wgsl_source.is_some(), "fused node carries WGSL");
        assert_eq!(fused.def.wires.len(), 2, "source→fused, fused→final_output");
        assert!(
            fused.def.wires.iter().any(|w| w.to_port == "src_0"),
            "an input wire targets the fused src_0 port"
        );
        assert!(
            fused.def.wires.iter().any(|w| w.from_port == "dst"),
            "the fused output wire leaves the dst port"
        );

        // Region topo order: gain(0) sat(1) hue(2) contrast(3) colorize(4)
        // mix(5) clamp(6). Spot-check the routing the binding rewrite depends on.
        let field_of = |nid: &str, p: &str| {
            fused
                .retarget
                .get(&(nid.into(), p.into()))
                .map(|(_, f)| f.clone())
        };
        assert_eq!(field_of("gain", "gain").as_deref(), Some("n0_gain"));
        assert_eq!(field_of("saturation", "saturation").as_deref(), Some("n1_saturation"));
        assert_eq!(field_of("hue", "hue").as_deref(), Some("n2_hue"));
        assert_eq!(field_of("contrast", "contrast").as_deref(), Some("n3_contrast"));
        assert_eq!(field_of("colorize", "focus").as_deref(), Some("n4_focus"));
        assert_eq!(field_of("grade_mix", "amount").as_deref(), Some("n5_amount"));
        assert_eq!(field_of("clamp", "max").as_deref(), Some("n6_max"));
        // 14 inner params across the 7 atoms (1+1+3+1+4+2+2).
        assert_eq!(fused.retarget.len(), 14);
        // All routed onto the single region's fused node.
        for (fused_id, _) in fused.retarget.values() {
            assert_eq!(fused_id.as_str(), "fused_region_0");
        }
    }

    /// Every seeded field name + every retarget target exists as a real param on
    /// the `WgslCompute` node once it reparses the generated source. The drift
    /// guard: if the codegen's `n{i}_{param}` field-naming convention diverges
    /// from the install-side reconstruction, the seeded params would land on
    /// non-existent fields and silently no-op — this catches it without a GPU.
    #[test]
    fn seeded_fields_match_wgsl_compute_params() {
        use manifold_node_engine::exec::effect_node::EffectNode;
        use manifold_node_engine::primitives::wgsl_compute::WgslCompute;
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");
        let node = fused
            .def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .unwrap();

        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(node.wgsl_source.as_deref().unwrap());
        let param_names: AHashSet<&str> =
            wc.parameters().iter().map(|p| p.name.as_ref()).collect();

        for field in node.params.keys() {
            assert!(
                param_names.contains(field.as_str()),
                "seeded field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
        for (_, field) in fused.retarget.values() {
            assert!(
                param_names.contains(field.as_str()),
                "retarget field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
    }
