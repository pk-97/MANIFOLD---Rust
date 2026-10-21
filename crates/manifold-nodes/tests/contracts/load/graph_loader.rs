mod tests {
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphWire, SerializedParamValue};
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

    /// A library-style preset (never passed project load) saved while the
    /// sky's sun had its own position: it loads with the World position gone
    /// and the Sun light wired into the bake, exactly as project load does.
    /// A `sun` input the document already wires stays as it is.
    #[test]
    fn library_preset_world_sun_migrates_at_instantiate() {
        let scene = |extra_wire: Option<serde_json::Value>| {
            let float = |v: f32| serde_json::json!({"type": "Float", "value": v});
            let mut wires = vec![
                serde_json::json!({"fromNode": 7, "fromPort": "out", "toNode": 8, "toPort": "light_0"}),
                serde_json::json!({"fromNode": 1, "fromPort": "envmap", "toNode": 8, "toPort": "envmap"}),
            ];
            wires.extend(extra_wire);
            let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
                "version": 1,
                "presetMetadata": {"id": "LibrarySky", "displayName": "Library Sky", "category": "Geometry",
                    "oscPrefix": "sky", "params": [{"id": "1_sun_x", "name": "Sun X", "min": -1.0, "max": 1.0, "defaultValue": 0.6}],
                    "bindings": [{"id": "1_sun_x", "label": "Sun X", "defaultValue": 0.6,
                        "target": {"kind": "node", "nodeId": "envmap", "param": "sun_x"}, "convert": {"type": "Float"}}]},
                "nodes": [
                    {"id": 1, "nodeId": "envmap", "typeId": "node.bake_environment", "handle": "envmap",
                     "params": {"mode": {"type": "Enum", "value": 1}, "sun_x": float(0.6), "sun_y": float(0.7),
                        "sun_z": float(0.4), "sun_disc_intensity": float(5.0)}},
                    {"id": 7, "nodeId": "sun", "typeId": "node.light", "handle": "sun",
                     "params": {"mode": {"type": "Enum", "value": 0}, "pos_y": float(5.0)}},
                    {"id": 9, "nodeId": "lamp", "typeId": "node.light", "handle": "lamp",
                     "params": {"mode": {"type": "Enum", "value": 0}}},
                    {"id": 8, "nodeId": "render", "typeId": "node.render_scene", "handle": "render",
                     "params": {"objects": {"type": "Int", "value": 0}, "lights": {"type": "Int", "value": 1}}}
                ],
                "wires": wires
            }))
            .unwrap();
            assert!(manifold_core::sky_sun::carries_world_sun(&def));
            let mut graph = Graph::new();
            instantiate_def(&mut graph, &def, &registry(), HandleScope::Global, BoundaryHandling::Standalone,
                &PreparedMeshRules::default())
                .unwrap_or_else(|e| panic!("library preset with a World sun must load: {e:?}"));
            let id = |handle: &str| graph.node_id_by_handle(handle).unwrap();
            let envmap = graph.get_node(id("envmap")).unwrap();
            for gone in ["sun_x", "sun_y", "sun_z"] {
                assert!(envmap.params.get(gone).is_none(), "{gone}");
            }
            assert_eq!(envmap.params.get("sun_disc_intensity"), Some(&ParamValue::Float(5.0)));
            let feeds: Vec<_> = graph.wires().iter().filter(|w| w.to == (id("envmap"), "sun")).map(|w| w.from.0).collect();
            (feeds, id("sun"), id("lamp"))
        };

        let (feeds, sun, _) = scene(None);
        assert_eq!(feeds, vec![sun], "the scene's Sun light feeds the sky");

        let user_wire = serde_json::json!({"fromNode": 9, "fromPort": "out", "toNode": 1, "toPort": "sun"});
        let (feeds, _, lamp) = scene(Some(user_wire));
        assert_eq!(feeds, vec![lamp], "an already-wired sun input is never overwritten");
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

    const WORLD_HANDLE: &str = "Box World";
    const BOX_BODY_HANDLE: &str = "Box Body";

    /// The bundled Physics Boxes as it was saved before the box spawner
    /// moved onto the copies body and before the sky's sun followed the Sun
    /// light: Count, Spacing in metres, Columns and Arrangement on the world
    /// with the cards driving them, the turn on the box transform, the
    /// world's one-group port names, and the bake with its own sun position and
    /// no `sun` wire.
    fn old_physics_boxes() -> EffectGraphDef {
        let mut doc: serde_json::Value =
            serde_json::from_str(include_str!("../../../assets/generator-presets/PhysicsBoxes.json")).unwrap();
        let nodes = doc["nodes"].as_array_mut().unwrap();
        let index = |nodes: &Vec<serde_json::Value>, handle: &str| {
            nodes.iter().position(|n| n["handle"] == handle).unwrap()
        };
        let body = index(nodes, BOX_BODY_HANDLE);
        for param in manifold_core::copies_body::COPY_PARAMS {
            nodes[body]["params"].as_object_mut().unwrap().remove(param);
        }
        nodes[body].as_object_mut().unwrap().remove("exposedParams");
        let transform = nodes.iter().position(|n| n["nodeId"] == "physics_demo_120").unwrap();
        for (axis, turn) in [("rot_x", 0.08), ("rot_y", 0.12), ("rot_z", 0.04)] {
            nodes[transform]["params"][axis]["value"] = turn.into();
        }
        let world = index(nodes, WORLD_HANDLE);
        nodes[world]["params"] = serde_json::json!({
            "copy_count": {"type": "Float", "value": 256},
            "copy_spacing": {"type": "Float", "value": 1.85},
            "copy_columns": {"type": "Float", "value": 24},
            "copy_layout": {"type": "Enum", "value": 1},
        });
        nodes[world]["exposedParams"] = serde_json::json!(["copy_count", "reset"]);
        let env = index(nodes, "environment");
        for (axis, v) in [("sun_x", -0.4), ("sun_y", 0.8), ("sun_z", 0.5)] {
            nodes[env]["params"][axis] = serde_json::json!({"type": "Float", "value": v});
        }
        doc["wires"].as_array_mut().unwrap().retain(|w| w["toPort"] != "sun");
        // Saved before copy groups: the world's one group has unnumbered ports.
        let world_id = doc["nodes"][world]["id"].clone();
        // The copies body's alias outputs were the world's own ports.
        for wire in doc["wires"].as_array_mut().unwrap() {
            if wire["fromPort"] == "instances" || wire["fromPort"] == "active_count" {
                wire["fromNode"] = world_id.clone();
                wire["fromPort"] = format!("{}_0", wire["fromPort"].as_str().unwrap()).into();
            }
        }
        for wire in doc["wires"].as_array_mut().unwrap() {
            for (node, port) in [("toNode", "toPort"), ("fromNode", "fromPort")] {
                let numbered = ["copies_0", "copies_acceleration_0", "instances_0", "active_count_0"];
                if wire[node] == world_id && numbered.iter().any(|name| wire[port] == *name) {
                    let old = wire[port].as_str().unwrap().trim_end_matches("_0").to_owned();
                    wire[port] = old.into();
                }
            }
        }
        for binding in doc["presetMetadata"]["bindings"].as_array_mut().unwrap() {
            if binding["id"].as_str().is_some_and(|id| id.starts_with("40_copy_")) {
                binding["target"]["nodeId"] = "physics_demo_40".into();
            }
            if binding["id"] == "40_copy_spacing" {
                binding["target"]["param"] = "copy_spacing".into();
                binding["defaultValue"] = 1.85.into();
            }
        }
        for param in doc["presetMetadata"]["params"].as_array_mut().unwrap() {
            if param["id"] == "40_copy_spacing" {
                param["defaultValue"] = 1.85.into();
            }
        }
        let def: EffectGraphDef = serde_json::from_value(doc).unwrap();
        for old in ["copies", "instances", "active_count"] {
            assert!(def.wires.iter().any(|w| w.to_port == old || w.from_port == old), "the old {old} port is saved");
        }
        assert!(manifold_core::copies_body::carries_world_copies(&def));
        assert!(manifold_core::sky_sun::carries_world_sun(&def));
        def
    }

    /// The old Physics Boxes builds with the same boxes: the four controls
    /// and their saved values on the copies body, none on the world, Spacing
    /// counted in box sizes, the transform's turn moved to Copy Turn, and the
    /// world's unnumbered copy ports bound as its first copy group's.
    fn assert_spawner_on_the_body(graph: &Graph) {
        let id = |handle: &str| graph.node_id_by_handle(handle).unwrap();
        let world_id = id(WORLD_HANDLE);
        for port in ["copies_0", "instances_0", "active_count_0"] {
            assert!(
                graph.wires().iter().any(|w| w.to == (world_id, port) || w.from == (world_id, port)),
                "the world's {port} is wired"
            );
        }
        let world = graph.get_node(world_id).unwrap();
        for param in ["copy_count", "copy_spacing", "copy_columns", "copy_layout"] {
            assert!(world.params.get(param).is_none(), "{param} left the world");
        }
        let body = graph.get_node(id(BOX_BODY_HANDLE)).unwrap();
        let number = |param: &str| match body.params.get(param) {
            Some(ParamValue::Float(v)) => *v,
            Some(ParamValue::Enum(v)) => *v as f32,
            other => panic!("{param}: {other:?}"),
        };
        assert_eq!(
            ["copy_count", "copy_size_spacing", "copy_columns", "copy_layout", "copy_turn_x", "copy_turn_y", "copy_turn_z"].map(number),
            [256.0, 1.06375, 24.0, 1.0, 0.08, 0.12, 0.04],
            "the same boxes: 1.85 m is 1.06375 box sizes, and each box keeps its turn"
        );
    }

    /// A library preset with the box spawner on its world (never passed
    /// project load) builds with the spawner on its copies body.
    #[test]
    fn library_preset_world_copies_migrate_at_instantiate() {
        let mut graph = Graph::new();
        instantiate_def(&mut graph, &old_physics_boxes(), &registry(), HandleScope::Global,
            BoundaryHandling::Standalone, &PreparedMeshRules::default())
            .unwrap_or_else(|e| panic!("old Physics Boxes must load: {e:?}"));
        assert_spawner_on_the_body(&graph);
    }

    /// Every generator builds through `from_def_for_render`, which strips
    /// retired params before it reaches `instantiate_def`. Both graph-shape
    /// migrations must run first there too: the sky's sun is wired from the
    /// Sun light, and the box spawner lands on the copies body.
    #[test]
    fn old_scene_migrates_through_from_def_for_render() {
        use manifold_node_engine::runtime::PresetRuntime;

        let runtime = PresetRuntime::from_def_for_render(old_physics_boxes(), &registry(), None, false)
            .unwrap_or_else(|e| panic!("old Physics Boxes must build: {e:?}"));
        let graph = &runtime.graph;
        let id = |handle: &str| graph.node_id_by_handle(handle).unwrap();
        let env = graph.get_node(id("environment")).unwrap();
        assert!(["sun_x", "sun_y", "sun_z"].iter().all(|p| env.params.get(*p).is_none()));
        let feeds: Vec<_> = graph.wires().iter().filter(|w| w.to == (id("environment"), "sun")).map(|w| w.from.0).collect();
        assert_eq!(feeds, vec![id("sun")], "the Sun light feeds the sky");
        assert_spawner_on_the_body(graph);
    }

    fn build(def: EffectGraphDef, what: &str) -> manifold_node_engine::runtime::PresetRuntime {
        manifold_node_engine::runtime::PresetRuntime::from_def_for_render(def, &registry(), None, false)
            .unwrap_or_else(|e| panic!("{what} must build: {e:?}"))
    }

    /// A copies body the load leaves as it was still builds: its metres
    /// Spacing, which the body no longer declares, counts box sizes.
    #[test]
    fn an_old_copies_body_left_as_it_was_still_builds() {
        let mut def = old_physics_boxes();
        let transform = def.nodes.iter_mut().find(|n| n.node_id == "physics_demo_120").unwrap();
        transform.params.insert("scale_y".into(), SerializedParamValue::Float { value: 2.0 });
        let runtime = build(def, "a body with a stretched transform");
        let graph = &runtime.graph;
        let body = graph.get_node(graph.node_id_by_handle(BOX_BODY_HANDLE).unwrap()).unwrap();
        assert!(body.params.get("copy_spacing").is_none());
        assert_eq!(body.params.get("copy_size_spacing"), Some(&ParamValue::Float(1.06375)), "1.85 m at the X scale");
    }

    /// A box transform that also places another node is split so that node
    /// keeps the turn; the copy takes a handle of its own, since the loader
    /// registers every handle once.
    #[test]
    fn a_shared_box_transform_splits_and_still_builds() {
        let mut def = old_physics_boxes();
        def.wires.push(EffectGraphWire { from_node: 120, from_port: "transform".into(), to_node: 124, to_port: "transform".into() });
        let runtime = build(def, "a shared box transform");
        let graph = &runtime.graph;
        let id = |handle: &str| graph.node_id_by_handle(handle).unwrap_or_else(|| panic!("{handle}"));
        let rot_x = |handle: &str| graph.get_node(id(handle)).unwrap().params.get("rot_x").cloned();
        assert_eq!(rot_x("Box Transform"), Some(ParamValue::Float(0.08)), "the other node keeps its turned transform");
        assert_eq!(rot_x("Box Transform copies"), Some(ParamValue::Float(0.0)));
        assert!(graph.wires().iter().any(|w| w.from.0 == id("Box Transform copies") && w.to == (id(BOX_BODY_HANDLE), "transform")));
        assert_spawner_on_the_body(graph);
    }

    /// A copies body saved with a metres Spacing that no copies port leads
    /// to has nothing to convert it against; the retired-param strip drops it
    /// so the layer still builds.
    #[test]
    fn an_unwired_body_with_a_stale_copy_spacing_still_builds() {
        let mut doc: serde_json::Value =
            serde_json::from_str(include_str!("../../../assets/generator-presets/PhysicsBoxes.json")).unwrap();
        let body = doc["nodes"].as_array().unwrap().iter().position(|n| n["handle"] == BOX_BODY_HANDLE).unwrap();
        doc["nodes"][body]["params"]["copy_spacing"] = serde_json::json!({"type": "Float", "value": 1.85});
        let body_id = doc["nodes"][body]["id"].clone();
        doc["wires"].as_array_mut().unwrap().retain(|w| !(w["fromNode"] == body_id && w["fromPort"] == "body"));
        let def: EffectGraphDef = serde_json::from_value(doc).unwrap();
        let runtime = build(def, "an unwired body with a stale Spacing");
        let graph = &runtime.graph;
        let body = graph.get_node(graph.node_id_by_handle(BOX_BODY_HANDLE).unwrap()).unwrap();
        assert!(body.params.get("copy_spacing").is_none());
    }

    /// The GPU FLIP domain sizes its own pool, so an old show's saved Max
    /// Particles, exposed or not, loads cleanly and is ignored: the bundled
    /// GPU FLIP Dam Break with a domain carrying 5000 builds, and the domain
    /// has no such param left.
    #[test]
    fn old_gpu_flip_max_particles_loads_and_is_ignored() {
        use manifold_node_engine::runtime::PresetRuntime;

        let mut doc: serde_json::Value = serde_json::from_str(include_str!(
            "../../../assets/generator-presets/WaterDamBreakGpuFlip.json"
        ))
        .unwrap();
        let mut stack = vec![&mut doc];
        let mut saved = 0;
        while let Some(value) = stack.pop() {
            if value["typeId"] == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID {
                value["params"]["particle_capacity"] = serde_json::json!({"type": "Int", "value": 5000});
                value["exposedParams"] = serde_json::json!(["particle_capacity"]);
                saved += 1;
            }
            match value {
                serde_json::Value::Array(items) => stack.extend(items.iter_mut()),
                serde_json::Value::Object(map) => stack.extend(map.values_mut()),
                _ => {}
            }
        }
        assert_eq!(saved, 1, "one GPU FLIP domain");
        let def: EffectGraphDef = serde_json::from_value(doc).unwrap();
        assert!(has_retired_params(&def));
        let runtime = PresetRuntime::from_def_for_render(def, &registry(), None, false)
            .unwrap_or_else(|e| panic!("an old GPU FLIP show must build: {e:?}"));
        let domains: Vec<_> = runtime.graph.nodes().filter(|node| node.node_id.as_str() == "domain").collect();
        assert_eq!(domains.len(), 1);
        assert!(domains[0].params.get("particle_capacity").is_none(), "Max Particles is gone");
        assert!(domains[0].exposed_params.iter().all(|param| &**param != "particle_capacity"));
    }
}
