mod tests {
    use manifold_node_engine::graph::Graph;
    use manifold_node_engine::load::expand::{
        PreparedModifierControlState, SceneModifierNodeCopy, SceneModifierNodeRoute,
    };
    use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use manifold_node_engine::state_store::{NodeState, StateStore};
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };

    fn local_graph(enable: bool) -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [{
                "id": 0,
                "nodeId": "local-gate",
                "typeId": "node.trigger_gate",
                "params": {"enable": {"type": "Bool", "value": enable}}
            }],
            "wires": []
        }))
        .expect("local modifier graph parses")
    }

    fn owner(ids_and_enable: &[(&str, bool)]) -> EffectGraphDef {
        EffectGraphDef {
            version: 3,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: ids_and_enable
                .iter()
                .map(|(id, enable)| SceneModifierInstanceDef {
                    id: NodeId::new(*id),
                    scene: SceneNodeRef {
                        scope: Vec::new(),
                        node: NodeId::new("scene"),
                    },
                    targets: SceneTargetSelection::AllObjects,
                    mesh_frames: Vec::new(),
                    legacy_math_view_carrier: None,
                    graph: Box::new(local_graph(*enable)),
                })
                .collect(),
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    fn runtime_graph(nodes: &[(&str, &str)]) -> Graph {
        let nodes = nodes
            .iter()
            .enumerate()
            .map(|(id, (node_id, type_id))| {
                serde_json::json!({
                    "id": id,
                    "nodeId": node_id,
                    "typeId": type_id
                })
            })
            .collect::<Vec<_>>();
        let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": nodes,
            "wires": []
        }))
        .expect("runtime graph parses");
        def.into_graph(&PrimitiveRegistry::with_builtin(), &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .expect("runtime graph builds")
    }

    fn routes(modifiers: &[(&str, &[&str])]) -> Vec<SceneModifierNodeRoute> {
        modifiers
            .iter()
            .map(|(modifier_id, generated)| SceneModifierNodeRoute {
                modifier_id: NodeId::new(*modifier_id),
                local: SceneNodeRef {
                    scope: Vec::new(),
                    node: NodeId::new("local-gate"),
                },
                copies: generated
                    .iter()
                    .map(|node_id| SceneModifierNodeCopy {
                        object: None,
                        node_id: NodeId::new(*node_id),
                    })
                    .collect(),
            })
            .collect()
    }

    struct Probe;
    impl NodeState for Probe {}

    #[test]
    fn modifier_control_state_unchanged_copies_survive_unrelated_add_and_reorder() {
        let prior_owner = owner(&[("a", true), ("b", true)]);
        let current_owner = owner(&[("b", true), ("a", true), ("c", true)]);
        let prior_routes = routes(&[("a", &["gen-a"]), ("b", &["gen-b"])]);
        let current_routes = routes(&[("b", &["gen-b"]), ("a", &["gen-a"]), ("c", &["gen-c"])]);
        let mut prior_graph = runtime_graph(&[
            ("gen-a", "node.trigger_gate"),
            ("gen-b", "node.trigger_gate"),
        ]);
        let mut graph = runtime_graph(&[
            ("gen-a", "node.trigger_gate"),
            ("gen-b", "node.trigger_gate"),
            ("gen-c", "node.trigger_gate"),
        ]);
        let prior =
            PreparedModifierControlState::prepare(&prior_owner, &prior_routes, &prior_graph)
                .unwrap();
        let current =
            PreparedModifierControlState::prepare(&current_owner, &current_routes, &graph).unwrap();
        let a_old = prior_graph
            .instance_by_node_id(&NodeId::new("gen-a"))
            .unwrap();
        let b_old = prior_graph
            .instance_by_node_id(&NodeId::new("gen-b"))
            .unwrap();
        let a_new = graph.instance_by_node_id(&NodeId::new("gen-a")).unwrap();
        let b_new = graph.instance_by_node_id(&NodeId::new("gen-b")).unwrap();
        let mut prior_state = StateStore::new();
        prior_state.insert(a_old, 0, Probe);
        prior_state.insert(b_old, 0, Probe);
        let mut state = StateStore::new();

        assert_eq!(
            current.harvest_from(
                &prior,
                &mut graph,
                &mut prior_graph,
                &mut state,
                &mut prior_state
            ),
            2
        );
        assert!(state.get::<Probe>(a_new, 0).is_some());
        assert!(state.get::<Probe>(b_new, 0).is_some());
        assert!(prior_state.is_empty());
    }

    #[test]
    fn modifier_control_state_changed_recipe_and_different_instance_do_not_cross() {
        let prior_owner = owner(&[("a", true)]);
        let changed_owner = owner(&[("a", false)]);
        let other_owner = owner(&[("other", true)]);
        let route = routes(&[("a", &["gen-a"])]);
        let other_route = routes(&[("other", &["gen-a"])]);
        let mut prior_graph = runtime_graph(&[("gen-a", "node.trigger_gate")]);
        let mut graph = runtime_graph(&[("gen-a", "node.trigger_gate")]);
        let prior =
            PreparedModifierControlState::prepare(&prior_owner, &route, &prior_graph).unwrap();
        let changed =
            PreparedModifierControlState::prepare(&changed_owner, &route, &graph).unwrap();
        let other =
            PreparedModifierControlState::prepare(&other_owner, &other_route, &graph).unwrap();
        let old_id = prior_graph
            .instance_by_node_id(&NodeId::new("gen-a"))
            .unwrap();
        let new_id = graph.instance_by_node_id(&NodeId::new("gen-a")).unwrap();
        let mut prior_state = StateStore::new();
        prior_state.insert(old_id, 0, Probe);
        let mut state = StateStore::new();
        assert_eq!(
            changed.harvest_from(
                &prior,
                &mut graph,
                &mut prior_graph,
                &mut state,
                &mut prior_state
            ),
            0
        );
        assert!(prior_state.get::<Probe>(old_id, 0).is_some());
        assert_eq!(
            other.harvest_from(
                &prior,
                &mut graph,
                &mut prior_graph,
                &mut state,
                &mut prior_state
            ),
            0
        );
        assert!(state.get::<Probe>(new_id, 0).is_none());
    }

    #[test]
    fn modifier_control_state_only_cpu_latches_and_all_target_copies_are_captured() {
        let graph = runtime_graph(&[
            ("gen-a1", "node.trigger_gate"),
            ("gen-a2", "node.trigger_gate"),
            ("gpu", "node.scale_offset_image"),
            ("plain", "node.value"),
        ]);
        let owner = owner(&[("a", true)]);
        let routes = routes(&[("a", &["gen-a1", "gen-a2", "gpu", "plain"])]);
        let prepared = PreparedModifierControlState::prepare(&owner, &routes, &graph).unwrap();
        assert_eq!(prepared.modifiers[0].nodes.len(), 2);
        assert!(
            prepared.modifiers[0]
                .nodes
                .iter()
                .all(|node| node.primitive_type == "node.trigger_gate")
        );
    }
}

