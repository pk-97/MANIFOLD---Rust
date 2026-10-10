mod tests {
use manifold_node_engine::graph::Graph;
    #[cfg(feature = "gpu-proofs")]
    fn built_in_coupled_splice_fixture() -> manifold_core::effect_graph_def::EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [
                {"id": 0, "nodeId": "source", "typeId": "system.source"},
                {"id": 1, "nodeId": "scene", "typeId": "node.render_scene",
                    "params": {"objects": {"type": "Int", "value": 2}}},
                {"id": 2, "nodeId": "final", "typeId": "system.final_output"},
                {"id": 3, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 4, "nodeId": "world_body", "typeId": "node.rigid_body"},
                {"id": 5, "nodeId": "fluid", "typeId": manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID},
                {"id": 6, "nodeId": "body_object", "typeId": "node.scene_object"},
                {"id": 7, "nodeId": "fluid_object", "typeId": "node.scene_object"}
            ],
            "wires": [
                {"fromNode": 4, "fromPort": "body", "toNode": 3, "toPort": "body_0"},
                {"fromNode": 3, "fromPort": "pose_0", "toNode": 6, "toPort": "transform"},
                {"fromNode": 6, "fromPort": "object", "toNode": 1, "toPort": "object_0"},
                {"fromNode": 5, "fromPort": "vertices", "toNode": 7, "toPort": "vertices"},
                {"fromNode": 7, "fromPort": "object", "toNode": 1, "toPort": "object_1"},
                {"fromNode": 1, "fromPort": "color", "toNode": 2, "toPort": "in"}
            ]
        }))
        .expect("built-in coupled splice fixture parses")
    }

    #[test]
    #[cfg(feature = "gpu-proofs")]
    fn repeated_coupled_splices_use_each_local_id_map() {
        use manifold_node_engine::scene::boundary_nodes::Source;
        use manifold_node_engine::load::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
        use manifold_node_engine::scene::mesh_change::PreparedMeshRules;
        use manifold_node_engine::persistence::PrimitiveRegistry;

        let def = built_in_coupled_splice_fixture();
        let registry = PrimitiveRegistry::with_cpu_flip_reference();
        let mut graph = Graph::new();
        let host_a = graph.add_node(Box::new(Source::new()));
        let host_b = graph.add_node(Box::new(Source::new()));
        let first = instantiate_def(
            &mut graph,
            &def,
            &registry,
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_a, "out"),
            },
            &PreparedMeshRules::default(),
        )
        .expect("first built-in splice instantiates");
        let second = instantiate_def(
            &mut graph,
            &def,
            &registry,
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_b, "out"),
            },
            &PreparedMeshRules::default(),
        )
        .expect("second built-in splice instantiates");

        let first_fluid = first.id_map[&5];
        let first_rigid = first.id_map[&3];
        let second_fluid = second.id_map[&5];
        let second_rigid = second.id_map[&3];
        assert_ne!(first_fluid, second_fluid);
        assert_ne!(first_rigid, second_rigid);
        assert_eq!(
            graph
                .node_pairs()
                .iter()
                .map(|pair| (pair.first, pair.second))
                .collect::<Vec<_>>(),
            vec![(first_fluid, first_rigid), (second_fluid, second_rigid)]
        );
    }
}
