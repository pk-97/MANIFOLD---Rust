mod tests {
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::load::graph_loader::{
        has_retired_params, instantiate_def, BoundaryHandling, GraphBuildError, HandleScope,
    };
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::parameters::ParamValue;
    use manifold_node_engine::scene::boundary_nodes::{FinalOutput, Source};
    use manifold_node_engine::scene::mesh_change::PreparedMeshRules;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    #[test]
    fn grouped_scene_object_from_old_phong_gets_scene_environment() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [
                {"id": 10, "nodeId": "object_group", "typeId": "group", "handle": "object_group",
                 "group": {
                    "interface": {"inputs": [], "outputs": [{"name": "object", "portType": "Object"}]},
                    "nodes": [
                        {"id": 11, "nodeId": "material", "typeId": "node.phong_material", "handle": "material"},
                        {"id": 12, "nodeId": "mesh", "typeId": "node.cube_mesh", "handle": "mesh"},
                        {"id": 13, "nodeId": "scene_object", "typeId": "node.scene_object", "handle": "scene_object"},
                        {"id": 14, "nodeId": "group_output", "typeId": "system.group_output", "handle": "output"}
                    ],
                    "wires": [
                        {"fromNode": 12, "fromPort": "vertices", "toNode": 13, "toPort": "vertices"},
                        {"fromNode": 11, "fromPort": "out", "toNode": 13, "toPort": "material"},
                        {"fromNode": 13, "fromPort": "object", "toNode": 14, "toPort": "object"}
                    ]
                 }},
                {"id": 20, "nodeId": "camera", "typeId": "node.camera_orbit", "handle": "camera"},
                {"id": 21, "nodeId": "render_scene", "typeId": "node.render_scene", "handle": "render_scene",
                 "params": {"objects": {"type": "Int", "value": 1}, "lights": {"type": "Int", "value": 0}}}
            ],
            "wires": [
                {"fromNode": 10, "fromPort": "object", "toNode": 21, "toPort": "object_0"},
                {"fromNode": 20, "fromPort": "out", "toNode": 21, "toPort": "camera"}
            ]
        })).unwrap();
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &PreparedMeshRules::default(),
        )
        .expect("grouped scene object gets an explicit environment");
        assert!(graph.node_id_by_handle("render_scene_phong_environment").is_some());
    }

    #[test]
    fn material_inspector_removed_mode_loads_and_invalid_mode_is_rejected() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                {
                    "id": 0,
                    "typeId": "node.pbr_material",
                    "params": { "coat_mode": { "type": "Enum", "value": 3 } }
                }
            ],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &PreparedMeshRules::default(),
        )
        .expect("removed material mode survives save/load");
        let invalid_json = json.replace("\"value\": 3", "\"value\": 4");
        let def: EffectGraphDef = serde_json::from_str(&invalid_json).expect("parse");
        let mut graph = Graph::new();
        let err = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &PreparedMeshRules::default(),
        )
        .expect_err("invalid material mode must be rejected at load time");
        assert_eq!(
            err,
            GraphBuildError::InvalidMaterialFeatureMode {
                node_id: 0,
                param: "coat_mode".to_string(),
                value: 4,
            }
        );
    }

    #[test]
    fn splice_rejects_param_type_mismatch() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));

        // Threshold.level is Float; this writes Bool.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                {
                    "id": 1,
                    "typeId": "node.threshold",
                    "params": { "level": { "type": "Bool", "value": true } }
                },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let result = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        &PreparedMeshRules::default());
        assert!(
            matches!(result, Err(GraphBuildError::ParamTypeMismatch { .. })),
            "splice should reject param type mismatch; got {result:?}",
        );
    }

    #[test]
    fn splice_folds_boundaries_and_returns_output_endpoint() {
        // A trivial 1-effect splice: source → threshold → final_output.
        // The chain graph's prev_node is the host source we connect to.
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));
        let host_final = graph.add_node(Box::new(FinalOutput::new()));

        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                { "id": 1, "typeId": "node.threshold", "handle": "thresh" },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let inst = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        &PreparedMeshRules::default())
        .expect("splice instantiates cleanly");

        // The two boundary nodes were folded.
        assert!(inst.final_output_id.is_none());
        assert!(inst.generator_input_id.is_none());

        // The threshold is the output endpoint.
        let (endpoint_node, endpoint_port) = inst.output_endpoint.expect("splice has endpoint");
        assert_eq!(endpoint_port, "out");
        let thresh_id = inst
            .id_map
            .get(&1)
            .copied()
            .expect("threshold id mapped");
        assert_eq!(endpoint_node, thresh_id);

        // Handle was returned locally, NOT registered on the graph.
        assert_eq!(inst.effect_local_handles.len(), 1);
        assert_eq!(inst.effect_local_handles[0].0.as_ref(), "thresh");
        assert!(graph.node_id_by_handle("thresh").is_none());

        // Wire host_source.out → thresh.source connected.
        let wires = graph.wires();
        assert!(
            wires.iter().any(|w| w.from.0 == host_source && w.to.0 == thresh_id),
            "source re-anchor wire missing; got wires: {wires:?}"
        );

        // host_source, host_final, threshold = 3 nodes. The def's
        // Source/FinalOutput were folded, never instantiated.
        assert_eq!(graph.node_count(), 3);
        let _ = host_final;
    }

    #[test]
    fn splice_audits_output_format_overrides() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));

        // Threshold's output format is hard-coded in its shader. An
        // outputFormats override against it must be rejected.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                {
                    "id": 1,
                    "typeId": "node.threshold",
                    "outputFormats": { "out": "rgba32float" }
                },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let result = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        &PreparedMeshRules::default());
        assert!(
            matches!(
                result,
                Err(GraphBuildError::OutputFormatNotSupported { .. })
            ),
            "splice should reject outputFormats against a hardcoded-format primitive; got {result:?}",
        );
    }

    #[test]
    fn standalone_instantiates_every_boundary() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        let inst = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        &PreparedMeshRules::default())
        .expect("standalone instantiates cleanly");
        assert!(inst.generator_input_id.is_some());
        assert!(inst.final_output_id.is_some());
        assert!(inst.output_endpoint.is_none());
        assert!(inst.effect_local_handles.is_empty());
        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.wires().len(), 1);
        assert!(graph.node_id_by_handle("uv").is_some());
    }

    #[test]
    fn ssao_from_depth_migrates_to_gtao() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                {
                    "id": 1,
                    "typeId": "node.ssao_from_depth",
                    "handle": "ssao",
                    "params": {
                        "radius": { "type": "Float", "value": 0.75 },
                        "intensity": { "type": "Float", "value": 1.25 },
                        "bias": { "type": "Float", "value": 0.025 }
                    }
                },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        &PreparedMeshRules::default())
        .expect("old id + stale `bias` param must still load after the D9(b) rename");

        let ssao_id = graph.node_id_by_handle("ssao").expect("ssao handle present");
        let inst = graph.get_node(ssao_id).expect("node exists");
        assert_eq!(inst.node.type_id().as_str(), "node.ssao_gtao", "resolves to the new atom");

        assert!(
            inst.node.parameters().iter().any(|p| p.name == "radius"),
            "radius param declared"
        );
        assert_eq!(
            inst.params.get("radius"),
            Some(&ParamValue::Float(0.75)),
            "radius carries the OLD document's stored value, not the descriptor default"
        );
        assert_eq!(
            inst.params.get("intensity"),
            Some(&ParamValue::Float(1.25)),
            "intensity carries over unchanged"
        );
        assert!(
            !inst.node.parameters().iter().any(|p| p.name == "bias"),
            "node.ssao_gtao declares no `bias` param (D9(b))"
        );
        assert!(
            !inst.params.contains_key("bias"),
            "the stale `bias` value from the old document must be dropped, not just \
             unreferenced by the descriptor — see migrate_def_type_ids's params.retain"
        );
    }

    #[test]
    fn standalone_old_phong_definition_is_migrated_before_instantiation() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [{
                "id": 1,
                "nodeId": "material",
                "typeId": "node.phong_material",
                "handle": "material",
                "params": {
                    "specular_power": {"type": "Float", "value": 32.0}
                }
            }],
            "wires": []
        }))
        .unwrap();
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &PreparedMeshRules::default(),
        )
        .expect("standalone legacy material loads through the migration");
        let id = graph.node_id_by_handle("material").expect("material handle");
        let node = graph.get_node(id).expect("material node");
        assert_eq!(node.node.type_id().as_str(), "node.pbr_material");
        assert!(node.params.get("roughness").is_some());
    }

    #[test]
    fn standalone_old_phong_render_mesh_gets_neutral_environment() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [
                {"id": 1, "nodeId": "material", "typeId": "node.phong_material", "handle": "material"},
                {"id": 2, "nodeId": "mesh", "typeId": "node.cube_mesh", "handle": "mesh"},
                {"id": 3, "nodeId": "camera", "typeId": "node.camera_orbit", "handle": "camera"},
                {"id": 4, "nodeId": "light", "typeId": "node.light", "handle": "light"},
                {"id": 5, "nodeId": "render", "typeId": "node.render_mesh", "handle": "render"}
            ],
            "wires": [
                {"fromNode": 2, "fromPort": "vertices", "toNode": 5, "toPort": "vertices"},
                {"fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "camera"},
                {"fromNode": 1, "fromPort": "out", "toNode": 5, "toPort": "material"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "light"}
            ]
        })).unwrap();
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &PreparedMeshRules::default(),
        )
        .expect("legacy Phong render graph gets an explicit neutral environment");
        let material_id = graph.node_id_by_handle("material").expect("material handle");
        assert_eq!(
            graph.get_node(material_id).unwrap().node.type_id().as_str(),
            "node.pbr_material"
        );
        let environment_id = graph
            .node_id_by_handle("render_phong_environment")
            .expect("migration-added environment handle");
        assert_eq!(
            graph.get_node(environment_id).unwrap().node.type_id().as_str(),
            "node.bake_environment"
        );
    }

    #[test]
    fn standalone_old_phong_render_scene_material_port_gets_neutral_environment() {
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [
                {"id": 1, "nodeId": "material", "typeId": "node.phong_material", "handle": "material"},
                {"id": 2, "nodeId": "mesh", "typeId": "node.cube_mesh", "handle": "mesh"},
                {"id": 3, "nodeId": "camera", "typeId": "node.camera_orbit", "handle": "camera"},
                {"id": 4, "nodeId": "light", "typeId": "node.light", "handle": "light"},
                {"id": 5, "nodeId": "render_scene", "typeId": "node.render_scene", "handle": "render_scene",
                 "params": {"objects": {"type": "Int", "value": 1}, "lights": {"type": "Int", "value": 1}}}
            ],
            "wires": [
                {"fromNode": 2, "fromPort": "vertices", "toNode": 5, "toPort": "mesh_0"},
                {"fromNode": 1, "fromPort": "out", "toNode": 5, "toPort": "material_0"},
                {"fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "camera"},
                {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "light_0"}
            ]
        })).unwrap();
        let mut graph = Graph::new();
        instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
            &PreparedMeshRules::default(),
        )
        .expect("legacy render_scene material port gets an explicit environment");
        let environment_id = graph
            .node_id_by_handle("render_scene_phong_environment")
            .expect("render_scene migration environment handle");
        assert_eq!(
            graph.get_node(environment_id).unwrap().node.type_id().as_str(),
            "node.bake_environment"
        );
    }

    #[test]
    fn retired_params_saved_nondefault_values_load() {
        let registry = registry();
        for &(type_id, param) in manifold_core::type_id_migration::RETIRED_PARAMS {
            let declared = registry.construct(type_id).expect("retired params belong to a live node type");
            assert!(declared.parameters().iter().all(|p| p.name != param), "{type_id}.{param} is still declared");
            let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
                "version": 1, "nodes": [{"id": 1, "nodeId": "n", "typeId": type_id, "handle": "n",
                    "params": {param: {"type": "Float", "value": 137.0}}}], "wires": []
            })).unwrap();
            assert!(has_retired_params(&def));
            let mut graph = Graph::new();
            instantiate_def(&mut graph, &def, &registry, HandleScope::Global,
                BoundaryHandling::Standalone, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
                .unwrap_or_else(|e| panic!("{type_id}.{param}: saved retired value must load: {e:?}"));
            let node = graph.get_node(graph.node_id_by_handle("n").unwrap()).unwrap();
            assert!(node.params.get(param).is_none(), "{type_id}.{param}");
        }
    }
}
