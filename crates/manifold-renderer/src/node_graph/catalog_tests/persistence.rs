use crate::node_graph::persistence::*;
use crate::node_graph::{Graph, ParamValue, Source, FinalOutput, SOURCE_TYPE_ID, FINAL_OUTPUT_TYPE_ID, compile, validate};
use crate::node_graph::primitives::{self, Blur, Threshold};
use std::collections::BTreeMap;
fn registry() -> PrimitiveRegistry { PrimitiveRegistry::with_builtin() }
    fn expect_err(result: Result<Graph, LoadError>) -> LoadError {
        match result {
            Ok(_) => panic!("expected LoadError, got Ok(Graph)"),
            Err(e) => e,
        }
    }


    #[test]
    fn builtin_registry_covers_every_shipped_primitive() {
        let r = registry();
        // Spot-check every category. Each primitive declares a public
        // `*_TYPE_ID` constant; if a new primitive is added without a
        // register call, this test fails.
        let expected: &[&str] = &[
            SOURCE_TYPE_ID,
            FINAL_OUTPUT_TYPE_ID,
            primitives::BRIGHTNESS_TYPE_ID,
            primitives::CHANNEL_MIX_TYPE_ID,
            primitives::COLOR_RAMP_TYPE_ID,
            primitives::MIX_TYPE_ID,
            primitives::THRESHOLD_TYPE_ID,
            primitives::BLUR_TYPE_ID,
            primitives::GAUSSIAN_BLUR_TYPE_ID,
            primitives::FEEDBACK_TYPE_ID,
            primitives::WET_DRY_TYPE_ID,
            primitives::WATERCOLOR_TYPE_ID,
        ];
        for id in expected {
            assert!(
                r.contains(id),
                "PrimitiveRegistry missing constructor for '{id}'"
            );
        }
    }

    #[test]
    fn round_trip_bloom_like_three_node_graph() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let thresh = g.add_node_named("thresh", Box::new(Threshold::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (out, "in")).unwrap();
        g.set_param(thresh, "level", ParamValue::Float(0.8))
            .unwrap();
        g.set_param(thresh, "softness", ParamValue::Float(0.05))
            .unwrap();

        // Serialize.
        let doc = GraphDocument::from_graph(&g);
        assert_eq!(doc.version, GRAPH_DOCUMENT_VERSION);
        assert_eq!(doc.nodes.len(), 3);
        assert_eq!(doc.wires.len(), 2);

        // Round-trip through JSON to catch missing serde derives.
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();

        let g2 = parsed.into_graph(&registry(), &crate::node_graph::mesh_change::PreparedMeshRules::default()).unwrap();
        assert_eq!(g2.node_count(), 3);
        assert_eq!(g2.wires().len(), 2);
        // Named handle survives.
        let thresh2 = g2
            .node_id_by_handle("thresh")
            .expect("handle 'thresh' round-tripped");
        let inst = g2.get_node(thresh2).unwrap();
        // Param value survives.
        assert_eq!(
            inst.params.get("level").cloned().unwrap(),
            ParamValue::Float(0.8)
        );
        assert_eq!(
            inst.params.get("softness").cloned().unwrap(),
            ParamValue::Float(0.05)
        );

        // Reloaded graph validates + compiles.
        validate(&g2).unwrap();
        let plan = compile(&g2).unwrap();
        assert!(!plan.steps().is_empty());
    }

    /// `exposed_params` on each `EffectGraphNode` round-trips through
    /// the JSON document. Confirms the graph editor's "Expose to card"
    /// state survives save → reload — the unified exposure source of
    /// truth (Step 1 + Step 2 of the exposure-unification plan).
    #[test]
    fn exposed_params_round_trip_through_json() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let thresh = g.add_node_named("thresh", Box::new(Threshold::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((_src, "out"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (out, "in")).unwrap();
        // Expose two params on the threshold node.
        g.set_param_exposed(thresh, "level", true).unwrap();
        g.set_param_exposed(thresh, "softness", true).unwrap();
        assert!(g.is_param_exposed(thresh, "level"));
        assert!(g.is_param_exposed(thresh, "softness"));

        // Serialize → deserialize.
        let doc = GraphDocument::from_graph(&g);
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();

        // Confirm the document carries the exposure set.
        let thresh_doc = parsed
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("thresh"))
            .unwrap();
        assert!(thresh_doc.exposed_params.contains("level"));
        assert!(thresh_doc.exposed_params.contains("softness"));

        // Confirm the live graph mirror picks them back up.
        let g2 = parsed.into_graph(&registry(), &crate::node_graph::mesh_change::PreparedMeshRules::default()).unwrap();
        let thresh2 = g2.node_id_by_handle("thresh").unwrap();
        assert!(g2.is_param_exposed(thresh2, "level"));
        assert!(g2.is_param_exposed(thresh2, "softness"));
        // Unset params remain unexposed.
        let final2 = g2.nodes().find(|n| n.id != thresh2).unwrap();
        assert!(!g2.is_param_exposed(final2.id, "any_param"));
    }

    #[test]
    fn unknown_param_is_a_clean_error() {
        let mut params = BTreeMap::new();
        params.insert(
            "totally_made_up".to_string(),
            SerializedParamValue::Float { value: 0.5 },
        );
        let doc = GraphDocument {
            scene_modifiers: Vec::new(),
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![NodeDocument {
                id: 0,
                node_id: manifold_core::NodeId::default(),
                type_id: primitives::THRESHOLD_TYPE_ID.to_string(),
                handle: None,
                params,
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
        let err = expect_err(doc.into_graph(&registry(), &crate::node_graph::mesh_change::PreparedMeshRules::default()));
        match err {
            LoadError::UnknownParam { param, .. } => assert_eq!(param, "totally_made_up"),
            other => panic!("expected UnknownParam, got {other:?}"),
        }
    }

    #[test]
    fn param_type_mismatch_is_a_clean_error() {
        let mut params = BTreeMap::new();
        // Threshold.level is a Float; we send an Enum.
        params.insert("level".to_string(), SerializedParamValue::Enum { value: 3 });
        let doc = GraphDocument {
            scene_modifiers: Vec::new(),
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![NodeDocument {
                id: 0,
                node_id: manifold_core::NodeId::default(),
                type_id: primitives::THRESHOLD_TYPE_ID.to_string(),
                handle: None,
                params,
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
        let err = expect_err(doc.into_graph(&registry(), &crate::node_graph::mesh_change::PreparedMeshRules::default()));
        match err {
            LoadError::ParamTypeMismatch { expected, got, .. } => {
                assert_eq!(expected, "Float");
                assert_eq!(got, "Enum");
            }
            other => panic!("expected ParamTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn decomposed_bloom_shape_round_trips() {
        // The same topology as the integration test in
        // primitives/mod.rs: Source → Blur → Mix.b (with Source also
        // fanning out to Mix.a) → FinalOutput. Verifies fan-out +
        // multi-input wires survive the round-trip.
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let blur = g.add_node(Box::new(Blur::new()));
        let mix = g.add_node(Box::new(primitives::Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (blur, "source")).unwrap();
        g.connect((src, "out"), (mix, "a")).unwrap();
        g.connect((blur, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();

        let doc = GraphDocument::from_graph(&g);
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();
        let g2 = parsed.into_graph(&registry(), &crate::node_graph::mesh_change::PreparedMeshRules::default()).unwrap();

        assert_eq!(g2.node_count(), 4);
        assert_eq!(g2.wires().len(), 4);
        validate(&g2).unwrap();
        let plan = compile(&g2).unwrap();
        assert_eq!(plan.steps().len(), 4);
    }
