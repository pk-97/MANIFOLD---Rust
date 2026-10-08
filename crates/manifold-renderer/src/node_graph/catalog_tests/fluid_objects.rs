
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
    use manifold_core::liquid_domain::is_liquid_domain;
    use manifold_core::NodeId;
    use manifold_nodes_scene::node_graph::scene_exposure::testkit::migrate_fluid_objects as migrate;
    use manifold_nodes_scene::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;
    const ROLE_SOURCE_TYPE_ID: &str = "node.fluid_role_source";
    use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
    use manifold_core::flatten::flatten_groups;

    const GPU_FLIP_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterDamBreakGpuFlip.json");
    const MATTER_DAM_BREAK_JSON: &str =
        include_str!("../../../assets/generator-presets/WaterDamBreakMatter.json");

    /// Role wires into any liquid domain, at any group depth.
    fn domain_role_inputs(def: &EffectGraphDef) -> usize {
        let flat = flatten_groups(def).expect("fixture graph flattens");
        flat.wires
            .iter()
            .filter(|wire| {
                wire.to_port.starts_with("role_")
                    && flat.nodes.iter().any(|node| {
                        node.id == wire.to_node && is_liquid_domain(&node.type_id)
                    })
            })
            .count()
    }

    fn project_with(def: &EffectGraphDef, preset: &'static str) -> (
        manifold_core::project::Project,
        manifold_core::GraphTarget,
    ) {
        use manifold_core::layer::Layer;
        use manifold_core::{GraphTarget, LayerId, PresetTypeId};

        let mut layer = Layer::new_generator("Dam".into(), PresetTypeId::new(preset), 0);
        let layer_id = LayerId::new("loose-obstacle-layer");
        layer.layer_id = layer_id.clone();
        let host = layer.gen_params_or_init();
        host.graph = Some(def.clone());
        host.refresh_manifest_from_graph();
        let mut project = manifold_core::project::Project::default();
        project.timeline.layers.push(layer);
        (project, GraphTarget::Generator(layer_id))
    }

    fn obstacle_slot(def: &EffectGraphDef) -> (u32, u32) {
        let render_id = def
            .nodes
            .iter()
            .find(|node| node.type_id == RENDER_SCENE_TYPE_ID)
            .expect("render scene")
            .id;
        let index = def
            .wires
            .iter()
            .find(|wire| {
                wire.to_node == render_id
                    && def.nodes.iter().any(|node| {
                        node.id == wire.from_node
                            && (node.handle.as_deref() == Some("Obstacle")
                                || node.node_id.as_str() == "obstacle_object")
                    })
            })
            .and_then(|wire| wire.to_port.strip_prefix("object_")?.parse().ok())
            .expect("Obstacle object slot");
        (render_id, index)
    }

    /// Remove Object on the obstacle through the real command, the way the
    /// Scene panel's delete issues it.
    fn delete_obstacle(def: EffectGraphDef, preset: &'static str) -> EffectGraphDef {
        use manifold_editing::command::Command;
        use manifold_editing::commands::graph::RemoveSceneObjectCommand;

        let (render_id, index) = obstacle_slot(&def);
        let (mut project, target) = project_with(&def, preset);
        let mut remove = RemoveSceneObjectCommand::new(target.clone(), vec![], render_id, index, def);
        remove.execute(&mut project);
        assert!(remove.was_applied(), "{:?}", remove.rejection_reason());
        project
            .graph_target_owner(&target)
            .and_then(|owner| owner.graph.clone())
            .expect("edited graph")
    }

    fn assert_obstacle_leaves_solver(def: EffectGraphDef, preset: &'static str) {
        assert_eq!(domain_role_inputs(&def), 1, "{preset}: the obstacle is the only body");
        let after = delete_obstacle(def, preset);
        assert_eq!(domain_role_inputs(&after), 0, "{preset}: deleted obstacle still in the solver");
        assert!(
            !after.nodes.iter().any(|node| {
                node.handle.as_deref() == Some("Obstacle")
                    || matches!(
                        node.node_id.as_str(),
                        "obstacle_collider" | "obstacle_transform" | "obstacle_object"
                    )
            }),
            "{preset}: the obstacle's nodes must go with it"
        );
        flatten_groups(&after).expect("edited graph flattens");
    }

    #[test]
    fn gpu_flip_dam_break_obstacle_groups_with_its_collider() {
        let mut def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        assert!(migrate(&mut def), "loose collider must group with its object");
        assert!(!migrate(&mut def), "migration must be idempotent");
        assert!(
            !def.nodes.iter().any(|node| node.type_id == ROLE_SOURCE_TYPE_ID),
            "the collider must live inside the Obstacle group"
        );
        flatten_groups(&def).expect("migrated graph flattens");
        let vm = SceneVm::from_def(&def).expect("scene discoverable");
        assert!(vm.objects.iter().any(|object| matches!(object,
            SceneObjectVm::Known(row) if row.name == "Obstacle" && row.group_node_id.is_some())));
    }

    #[test]
    fn deleting_grouped_gpu_flip_obstacle_removes_its_collider() {
        let mut def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        migrate(&mut def);
        assert_obstacle_leaves_solver(def, "WaterDamBreakGpuFlip");
    }

    /// The command owns the rule, not the migration: a loose object's
    /// collider goes with it.
    #[test]
    fn deleting_loose_gpu_flip_obstacle_removes_its_collider() {
        let def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        assert_obstacle_leaves_solver(def, "WaterDamBreakGpuFlip");
    }

    #[test]
    fn deleting_matter_dam_break_obstacle_removes_its_collider() {
        let def: EffectGraphDef =
            serde_json::from_str(MATTER_DAM_BREAK_JSON).expect("preset parses");
        assert_obstacle_leaves_solver(def, "WaterDamBreakMatter");
    }

    #[test]
    fn enabling_physics_on_loose_obstacle_with_a_collider_is_refused() {
        use manifold_editing::command::Command;
        use manifold_editing::commands::graph::EnableSceneObjectPhysicsCommand;

        let def: EffectGraphDef =
            serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        let (render_id, index) = obstacle_slot(&def);
        let (mut project, target) = project_with(&def, "WaterDamBreakGpuFlip");
        let mut enable =
            EnableSceneObjectPhysicsCommand::new(target, render_id, index, Vec::new(), def);
        enable.execute(&mut project);
        assert!(!enable.was_applied());
        let reason = enable.rejection_reason().unwrap_or_default();
        assert!(reason.contains("Fluid Role"), "{reason}");
    }

    /// A loose collider that cannot group must not stop the others.
    #[test]
    fn ungroupable_loose_collider_does_not_block_the_rest() {
        let def: EffectGraphDef = serde_json::from_str(GPU_FLIP_DAM_BREAK_JSON).expect("preset parses");
        // Add the second collider directly to the solver topology.
        let mut def = flatten_groups(&def).expect("preset flattens");
        let mut blocked = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "obstacle_object")
            .unwrap()
            .clone();
        let ids = max_node_id_recursive(&def.nodes);
        let mut collider = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "obstacle_collider")
            .unwrap()
            .clone();
        let transform = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "obstacle_transform")
            .unwrap()
            .clone();
        // A handle-less object cannot be grouped; it is listed first.
        blocked.id = ids + 1;
        blocked.node_id = NodeId::new("blocked_object");
        blocked.handle = None;
        collider.id = ids + 2;
        collider.node_id = NodeId::new("blocked_collider");
        let mut blocked_transform = transform.clone();
        blocked_transform.id = ids + 3;
        blocked_transform.node_id = NodeId::new("blocked_transform");
        let domain = def
            .nodes
            .iter()
            .find(|node| node.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
            .unwrap()
            .id;
        let render = def
            .nodes
            .iter()
            .find(|node| node.type_id == RENDER_SCENE_TYPE_ID)
            .unwrap()
            .id;
        for (from_node, from_port, to_node, to_port) in [
            (ids + 3, "transform", ids + 1, "transform"),
            (ids + 3, "transform", ids + 2, "transform"),
            (ids + 2, "role", domain, "role_1"),
            (ids + 1, "object", render, "object_10"),
        ] {
            def.wires.push(EffectGraphWire {
                from_node,
                from_port: from_port.into(),
                to_node,
                to_port: to_port.into(),
            });
        }
        def.nodes.insert(0, blocked);
        def.nodes.insert(0, collider);
        def.nodes.push(blocked_transform);
        assert!(migrate(&mut def));
        let root_roles: Vec<_> = def
            .nodes
            .iter()
            .filter(|node| node.type_id == ROLE_SOURCE_TYPE_ID)
            .map(|node| node.node_id.as_str())
            .collect();
        assert_eq!(root_roles, ["blocked_collider"], "the groupable obstacle must still group");
    }

fn max_node_id_recursive(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|node| {
            node.id.max(
                node.group
                    .as_deref()
                    .map(|group| max_node_id_recursive(&group.nodes))
                    .unwrap_or(0),
            )
        })
        .max()
        .unwrap_or(0)
}
