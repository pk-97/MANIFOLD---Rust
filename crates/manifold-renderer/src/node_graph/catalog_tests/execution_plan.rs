use manifold_node_engine::{graph::Graph, exec::execution_plan::compile};

    #[test]
    fn provided_texture_outputs_are_held_but_feedback_back_edges_stay_writable() {
        use manifold_node_engine::scene::boundary_nodes::FinalOutput;
        use {manifold_nodes_scene::node_graph::primitives::gltf_texture_source::GltfTextureSource, manifold_nodes_image::node_graph::primitives::Feedback};
        for feedback in [false, true] {
            let mut graph = Graph::new();
            let source = graph.add_node(Box::new(GltfTextureSource::new()));
            let output = graph.add_node(Box::new(FinalOutput::new()));
            if feedback {
                let history = graph.add_node(Box::new(Feedback::new()));
                graph.connect((source, "out"), (history, "in")).unwrap();
                graph.connect((history, "out"), (output, "in")).unwrap();
            } else {
                graph.connect((source, "out"), (output, "in")).unwrap();
            }
            let plan = compile(&graph).unwrap();
            let resource = plan.steps().iter().find(|step| step.node == source).unwrap().outputs[0].1;
            assert_eq!(plan.is_provided_texture(resource), !feedback);
            assert_eq!(plan.persistent_resources().contains(&resource), feedback);
            assert_eq!(plan.held_resources().contains(&resource), !feedback);
            assert!(plan.steps().iter().all(|step| !step.free_after.contains(&resource)));
        }
    }
