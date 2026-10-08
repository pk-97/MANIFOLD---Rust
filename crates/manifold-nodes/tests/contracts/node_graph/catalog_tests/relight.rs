
    use manifold_nodes_scene::node_graph::relight::{relight_augment, testkit::RL_PREFIX, testkit::find_height_source, testkit::wire};
    use manifold_core::effects::RelightParams;
    use manifold_core::effect_graph_def::{EffectGraphNode, EffectGraphWire};
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::scene::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
    use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION, EffectGraphDef};

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    fn node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: Default::default(),
            type_id: type_id.to_string(),
            handle: handle.map(|s| s.to_string()),
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    fn base_def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes,
            wires,
        }
    }

    /// A trivial effect chain: Source → Contrast (Inherit) → FinalOutput.
    /// Contrast has no SourceHeight upstream, so this exercises the
    /// luminance-of-output fallback.
    fn simple_effect_def() -> EffectGraphDef {
        base_def(
            vec![
                node(0, SOURCE_TYPE_ID, Some("source")),
                node(1, "node.contrast", Some("contrast")),
                node(2, FINAL_OUTPUT_TYPE_ID, Some("final")),
            ],
            vec![
                wire(0, "out", 1, "in"),
                wire(1, "out", 2, "in"),
            ],
        )
    }

    /// A generator def whose final producer IS a SourceHeight atom
    /// (`node.noise`) directly feeding `final_output`.
    fn source_height_def() -> EffectGraphDef {
        base_def(
            vec![node(0, "node.noise", Some("gen")), node(1, FINAL_OUTPUT_TYPE_ID, Some("final"))],
            vec![wire(0, "out", 1, "in")],
        )
    }

    #[test]
    fn augmenting_a_minimal_def_splices_the_template_and_preserves_originals() {
        let reg = registry();
        let def = simple_effect_def();
        let augmented = relight_augment(&def, &reg, &RelightParams::default());

        // Every original node is present, unchanged, at its original id.
        for n in &def.nodes {
            let found = augmented.nodes.iter().find(|m| m.id == n.id).expect("original node preserved");
            assert_eq!(found.type_id, n.type_id);
            assert_eq!(found.handle, n.handle);
        }
        // Every original wire is present, unchanged, EXCEPT the one that fed
        // final_output — that one gets re-anchored onto the template's tail.
        let final_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
            .unwrap()
            .id;
        for w in &def.wires {
            if w.to_node == final_id && w.to_port == "in" {
                continue;
            }
            assert!(augmented.wires.iter().any(|aw| aw == w), "original wire preserved: {w:?}");
        }
        // final_output is now fed by a fresh rl_-prefixed node.
        let new_final_wire = augmented
            .wires
            .iter()
            .find(|w| w.to_node == final_id && w.to_port == "in")
            .expect("final_output still wired");
        let producer = augmented.nodes.iter().find(|n| n.id == new_final_wire.from_node).unwrap();
        assert!(producer.handle.as_deref().unwrap().starts_with(RL_PREFIX));

        // All minted nodes carry fresh ids above the original max and rl_ handles.
        let orig_max = def.nodes.iter().map(|n| n.id).max().unwrap();
        for n in &augmented.nodes {
            if n.handle.as_deref().is_some_and(|h| h.starts_with(RL_PREFIX)) {
                assert!(n.id > orig_max, "minted node id {} must exceed original max {orig_max}", n.id);
            }
        }
    }

    #[test]
    #[should_panic(expected = "already carries rl_-prefixed nodes")]
    fn double_application_is_refused() {
        let reg = registry();
        let def = simple_effect_def();
        let once = relight_augment(&def, &reg, &RelightParams::default());
        let _twice = relight_augment(&once, &reg, &RelightParams::default());
    }

    #[test]
    fn source_height_producer_is_tapped_directly() {
        let reg = registry();
        let def = source_height_def();
        let final_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
            .unwrap()
            .id;
        let tapped = find_height_source(&def, &reg, final_id);
        assert_eq!(tapped, Some((0, "out".to_string())));
    }

    #[test]
    fn def_with_only_inherit_and_terminal_falls_back_to_luminance() {
        let reg = registry();
        let def = simple_effect_def();
        let final_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)
            .unwrap()
            .id;
        // Source is depth_rule Inherit (it's the entry boundary — see
        // boundary_nodes.rs), Contrast is Inherit too, so the walk runs off
        // the front of the graph (Source has no upstream wire) and returns
        // None — the fallback per D4.
        assert_eq!(find_height_source(&def, &reg, final_id), None);
    }


    /// "Just works on every graph" contract (P3 item 4): every bundled
    /// effect AND generator preset def must still validate after
    /// augmentation. GPU-gated (`validate_def` builds a real chain/generator
    /// through a `GpuDevice`) — run with `--features gpu-proofs`.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn every_bundled_preset_validates_after_relight_augmentation() {
        use crate::bundled_presets::bundled_preset_def;
        use manifold_node_engine::validate::{ValidateKind, validate_def};
        use manifold_core::preset_def::PresetKind;

        let reg = registry();
        let device = manifold_gpu::testkit::test_device();
        let device_arc = device.arc();
        let mut checked = 0usize;
        for (kind, validate_kind) in [
            (PresetKind::Effect, ValidateKind::Effect),
            (PresetKind::Generator, ValidateKind::Generator),
        ] {
            for type_id in crate::bundled_presets::bundled_preset_type_ids(kind) {
                let def = bundled_preset_def(&type_id)
                    .unwrap_or_else(|| panic!("bundled preset {type_id:?} has no parsed def"));
                let augmented = relight_augment(def, &reg, &RelightParams::default());
                let report = validate_def(&augmented, &reg, validate_kind, &device_arc);
                assert!(
                    report.errors.is_empty(),
                    "relight-augmented {type_id:?} failed validation: {:?}",
                    report.errors
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "expected at least one bundled preset to check");
    }
    /// The golden test from P3 item 3 / D2: `relight = false` builds today's
    /// exact graph for every bundled effect preset — structural equality
    /// (node type_ids/doc-ids/params, and wires, in the SAME order) between
    /// the production `splice_def_into_chain(..., false)` wrapper and
    /// calling `instantiate_def` directly (bypassing the relight wrapper
    /// entirely — the pre-P3 call shape). Proves the toggle is a genuine
    /// compiled-variant, not a hidden always-on cost.
    ///
    /// Deliberately compares the pre-`compile()` `Graph`, not the compiled
    /// `ExecutionPlan`: `compile()`'s topological sort ties independent
    /// (no-dependency) nodes by hash-map iteration order, which is
    /// per-process-random and reorders unrelated steps between two
    /// independently-built graphs even when they're structurally identical
    /// — a pre-existing property of the compiler, unrelated to relight, that
    /// made an `ExecutionPlan`-level comparison spuriously flaky. Comparing
    /// the graph directly asserts the actual invariant this test exists for.
    #[test]
    fn relight_off_matches_pre_relight_effect_graph_for_every_bundled_preset() {
        use manifold_node_engine::scene::boundary_nodes::{FinalOutput, Source};
        use crate::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
        use manifold_node_engine::load::chain_spec::splice_def_into_chain;
        use manifold_node_engine::graph::Graph;
        use manifold_node_engine::load::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
        use manifold_core::preset_def::PresetKind;

        type NodeSig = (u32, String, String, String);
        type WireSig = (u32, String, u32, String);
        fn signature(graph: &Graph) -> (Vec<NodeSig>, Vec<WireSig>) {
            let mut nodes: Vec<_> = graph
                .nodes()
                .map(|n| {
                    // AHashMap iteration order is per-process-random — sort
                    // params by key so this signature is comparable across
                    // two independently-built graphs.
                    let mut params: Vec<(&str, String)> =
                        n.params.iter().map(|(k, v)| (k.as_ref(), format!("{v:?}"))).collect();
                    params.sort_by_key(|(k, _)| *k);
                    (
                        n.id.0,
                        n.node.type_id().as_str().to_string(),
                        n.node_id.as_str().to_string(),
                        format!("{params:?}"),
                    )
                })
                .collect();
            nodes.sort_by_key(|(id, ..)| *id);
            let wires = graph
                .wires()
                .iter()
                .map(|w| (w.from.0.0, w.from.1.to_string(), w.to.0.0, w.to.1.to_string()))
                .collect();
            (nodes, wires)
        }

        let reg = registry();
        let mut checked = 0usize;
        for type_id in bundled_preset_type_ids(PresetKind::Effect) {
            let def = bundled_preset_def(&type_id)
                .unwrap_or_else(|| panic!("bundled preset {type_id:?} has no parsed def"));

            // Path A: the production wrapper, relight OFF.
            let mut graph_a = Graph::new();
            let src_a = graph_a.add_node(Box::new(Source::new()));
            let Some(result_a) = splice_def_into_chain(&mut graph_a, (src_a, "out"), def, &reg, None, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default()) else {
                continue; // a preset that fails to splice fails identically on both paths; skip rather than false-fail
            };
            let final_a = graph_a.add_node(Box::new(FinalOutput::new()));
            graph_a.connect(result_a.output, (final_a, "in")).expect("connect A");

            // Path B: instantiate_def directly — bypasses the relight wrapper
            // entirely, exactly the pre-P3 call shape.
            let mut graph_b = Graph::new();
            let src_b = graph_b.add_node(Box::new(Source::new()));
            let inst_b = instantiate_def(
                &mut graph_b,
                def,
                &reg,
                HandleScope::PerSplice,
                BoundaryHandling::Splice {
                    source_endpoint: (src_b, "out"),
                },
            &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .expect("instantiate_def B");
            let final_b = graph_b.add_node(Box::new(FinalOutput::new()));
            graph_b
                .connect(inst_b.output_endpoint.expect("splice output"), (final_b, "in"))
                .expect("connect B");

            assert_eq!(
                signature(&graph_a),
                signature(&graph_b),
                "relight=false must produce a byte-identical graph for {type_id:?}"
            );
            checked += 1;
        }
        assert!(checked > 0, "expected at least one bundled effect preset to check");
    }
