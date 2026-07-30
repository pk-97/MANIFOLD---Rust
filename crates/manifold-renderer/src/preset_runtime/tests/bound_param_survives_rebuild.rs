    //! BUG-ji6q regression: a card-stamped binding's `default_value` is a
    //! frozen snapshot of `node.params` taken at stamp time
    //! (`stamp_scene_node_exposures_into`). `instantiate_def` correctly
    //! writes the def's `node.params` onto the live node every rebuild, but
    //! `BoundGraph::new` then calls `apply_binding_defaults`, which
    //! unconditionally replants every binding's `default_value` back over
    //! the top. Two copies of one fact, nothing keeping them in sync: any
    //! writer that touches `node.params` for a bound target (def edits,
    //! migrations, direct macro/mapping writes) is silently reverted on the
    //! very next rebuild. CPU-only — no `GpuDevice`, no `PresetRuntime`.
    use std::collections::BTreeMap;

    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, SerializedParamValue,
    };
    use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

    use crate::node_graph::scene_exposure::metadata_for_node_type;
    use crate::node_graph::{
        BindingSource, BoundaryHandling, Graph, HandleScope, NodeInstanceId, ParamValue,
        PrimitiveRegistry, ResolvedBinding, ResolvedTarget, instantiate_def,
    };

    /// A single `node.bake_environment` node, its `intensity` param stamped
    /// to `stamped_intensity` — the card-stamped def a GLB import produces.
    fn def_with_stamped_bake_environment(stamped_intensity: f32) -> (EffectGraphDef, f32) {
        let node_id = NodeId::new("env");
        let metadata = metadata_for_node_type("node.bake_environment");
        assert!(
            !metadata.is_empty(),
            "node.bake_environment must be a known primitive for this repro to mean anything"
        );

        // `stamp_scene_node_exposures_into` seeds each binding's
        // `default_value` from the node's ALREADY-stamped param when
        // present (BUG-303's fix — an importer-placed value survives the
        // stamp), falling back to the primitive manifest default only when
        // absent. So the GLB-import shape is: `node_params` already carries
        // the imported value at stamp time, and the binding freezes THAT.
        let mut node_params = BTreeMap::new();
        node_params.insert(
            "intensity".to_string(),
            SerializedParamValue::Float { value: stamped_intensity },
        );

        let mut params = Vec::new();
        let mut bindings = Vec::new();
        stamp_scene_node_exposures_into(
            &mut params,
            &mut bindings,
            1,
            &node_id,
            "node.bake_environment",
            "Environment",
            &metadata,
            &node_params,
        );
        let binding = bindings
            .iter()
            .find(|b| matches!(&b.target, manifold_core::effect_graph_def::BindingTarget::Node { param, .. } if param == "intensity"))
            .expect("intensity binding stamped");
        let frozen_default = binding.default_value;

        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: node_id.clone(),
                type_id: "node.bake_environment".to_string(),
                handle: Some("env".to_string()),
                params: node_params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };
        (def, frozen_default)
    }

    fn intensity_of(graph: &Graph, inst: NodeInstanceId) -> ParamValue {
        graph
            .get_node(inst)
            .and_then(|n| n.params.get("intensity").cloned())
            .expect("bake_environment exposes `intensity`")
    }

    fn build(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> (Graph, NodeInstanceId) {
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            def,
            registry,
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .expect("single bake_environment node instantiates cleanly");
        let inst = graph
            .instance_by_node_id(&NodeId::new("env"))
            .expect("env node registered under its stable NodeId");
        (graph, inst)
    }

    #[test]
    fn bound_param_write_survives_a_graph_rebuild() {
        let registry = PrimitiveRegistry::with_builtin();

        // Stamp at intensity = 2.0 (frozen into the binding's default_value)
        // and give the def's live node.params that same 2.0 so the FIRST
        // build is unambiguous.
        let (mut def, frozen_default) = def_with_stamped_bake_environment(2.0);
        assert_eq!(
            frozen_default, 2.0,
            "sanity: the binding's frozen default must equal the stamped value"
        );

        let (graph, inst) = build(&def, &registry);
        assert_eq!(
            intensity_of(&graph, inst),
            ParamValue::Float(2.0),
            "sanity: instantiate_def wrote the def's stamped node.params onto the live node"
        );

        // A direct `node.params` writer — a def edit, a migration, a macro
        // write — touches the LIVE node's `intensity` to 5.0 AFTER the
        // stamp. This mirrors what happens on stage: the card-stamped
        // default (2.0) never moves, but the actual value the user set
        // (5.0) is what must survive.
        def.nodes[0].params.insert(
            "intensity".to_string(),
            SerializedParamValue::Float { value: 5.0 },
        );

        // Rebuild the SAME def (now carrying node.params.intensity = 5.0)
        // into a fresh graph — the way any graph rebuild does — then run
        // the binding lifecycle a second time via `BoundGraph::new`, the
        // exact call every effect/generator rebuild makes.
        let (mut rebuilt_graph, rebuilt_inst) = build(&def, &registry);
        assert_eq!(
            intensity_of(&rebuilt_graph, rebuilt_inst),
            ParamValue::Float(5.0),
            "sanity: instantiate_def alone (no BoundGraph) preserves the write"
        );

        let binding = ResolvedBinding {
            id: std::borrow::Cow::Borrowed("intensity"),
            label: std::borrow::Cow::Borrowed("Intensity"),
            default_value: frozen_default,
            target: ResolvedTarget::Node {
                node: rebuilt_inst,
                param: std::borrow::Cow::Borrowed("intensity"),
            },
            convert: crate::node_graph::ParamConvert::Float,
            source: BindingSource::Static,
            source_id: std::borrow::Cow::Borrowed("intensity"),
            reshape: None,
            wraps_angle: false,
        };
        // This is the call every effect/generator rebuild makes:
        // `BoundGraph::new` → `apply_binding_defaults`.
        let _bound = crate::node_graph::BoundGraph::new(vec![binding], &mut rebuilt_graph);

        assert_eq!(
            intensity_of(&rebuilt_graph, rebuilt_inst),
            ParamValue::Float(5.0),
            "BUG-ji6q: BoundGraph::new's apply_binding_defaults must not clobber a \
             bound param write that happened after the card was stamped. Expected \
             intensity to survive the rebuild at 5.0 (the direct node.params write \
             that instantiate_def correctly applied); found it reverted to the \
             binding's frozen stamp-time default_value (2.0) instead — \
             apply_binding_defaults unconditionally replants default_value over \
             whatever instantiate_def just wrote from node.params."
        );
    }
