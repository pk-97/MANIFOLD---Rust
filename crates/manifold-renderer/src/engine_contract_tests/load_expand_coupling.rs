mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
    use manifold_core::liquid_domain::{GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};

    use manifold_node_engine::load::expand::{
        coupling::{prepare_coupled_scenes, CoupledSceneBinding},
        SceneModifierExpandError,
    };
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::water::physics::RigidImpulseTargets;

    struct GraphBuilder {
        next_id: u32,
        nodes: Vec<EffectGraphNode>,
        wires: Vec<EffectGraphWire>,
    }

    impl GraphBuilder {
        fn new() -> Self {
            Self {
                next_id: 1,
                nodes: Vec::new(),
                wires: Vec::new(),
            }
        }

        fn node(&mut self, stable: impl Into<String>, type_id: &str) -> u32 {
            let stable = stable.into();
            if let Some(node) = self
                .nodes
                .iter()
                .find(|node| node.node_id.as_str() == stable)
            {
                return node.id;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.nodes.push(EffectGraphNode {
                id,
                node_id: NodeId::new(stable.clone()),
                type_id: type_id.into(),
                handle: Some(stable),
                params: BTreeMap::new(),
                exposed_params: BTreeSet::new(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            });
            id
        }

        fn wire(&mut self, from: u32, from_port: &str, to: u32, to_port: &str) {
            if self.wires.iter().any(|wire| {
                wire.from_node == from
                    && wire.from_port == from_port
                    && wire.to_node == to
                    && wire.to_port == to_port
            }) {
                return;
            }
            self.wires.push(EffectGraphWire {
                from_node: from,
                from_port: from_port.into(),
                to_node: to,
                to_port: to_port.into(),
            });
        }

        fn pair_objects(
            &mut self,
            scene: u32,
            scene_name: &str,
            fluid_name: &str,
            world_name: &str,
            slots: &[usize],
            copies: bool,
            object_index: &mut usize,
        ) {
            let world = self.node(world_name, "node.physics_world");
            let body = self.node(format!("{world_name}_body"), "node.rigid_body");
            let fluid = self.fluid_mesh(fluid_name);
            for &slot in slots {
                self.wire(body, "body", world, &format!("body_{slot}"));
                let object = self.node(
                    format!("{scene_name}_body_{world_name}_{object_index}"),
                    "node.scene_object",
                );
                *object_index += 1;
                self.wire(world, &format!("pose_{slot}"), object, "transform");
                self.wire(object, "object", scene, &format!("object_{object_index}"));
            }
            if copies {
                self.wire(body, "body", world, "copies");
                let object = self.node(
                    format!("{scene_name}_copies_{world_name}_{object_index}"),
                    "node.scene_object",
                );
                *object_index += 1;
                self.wire(world, "instances", object, "instances");
                self.wire(object, "object", scene, &format!("object_{object_index}"));
            }
            let object = self.node(
                format!("{scene_name}_fluid_{fluid_name}_{object_index}"),
                "node.scene_object",
            );
            *object_index += 1;
            self.wire(fluid, "vertices", object, "vertices");
            self.wire(object, "object", scene, &format!("object_{object_index}"));
        }

        fn scene(&mut self, scene_name: &str, pairs: &[(&str, &str, &[usize], bool)]) -> u32 {
            let scene = self.node(scene_name, "node.render_scene");
            let mut object_index = 0;
            for &(fluid, world, slots, copies) in pairs {
                self.pair_objects(
                    scene,
                    scene_name,
                    fluid,
                    world,
                    slots,
                    copies,
                    &mut object_index,
                );
            }
            scene
        }

        fn render_only_scene(&mut self, scene_name: &str) {
            let scene = self.node(scene_name, "node.render_scene");
            let mesh = self.node(format!("{scene_name}_mesh"), "node.cube_mesh");
            let object = self.node(format!("{scene_name}_object"), "node.scene_object");
            self.wire(mesh, "vertices", object, "vertices");
            self.wire(object, "object", scene, "object_0");
        }

        fn rigid_only_scene(&mut self, scene_name: &str, world_name: &str, slots: &[usize]) {
            let scene = self.node(scene_name, "node.render_scene");
            let world = self.node(world_name, "node.physics_world");
            let body = self.node(format!("{world_name}_body"), "node.rigid_body");
            for (object_index, &slot) in slots.iter().enumerate() {
                self.wire(body, "body", world, &format!("body_{slot}"));
                let object = self.node(
                    format!("{scene_name}_body_{world_name}_{object_index}"),
                    "node.scene_object",
                );
                self.wire(world, &format!("pose_{slot}"), object, "transform");
                self.wire(object, "object", scene, &format!("object_{object_index}"));
            }
        }

        fn fluid_only_scene(&mut self, scene_name: &str, fluid_name: &str) {
            let scene = self.node(scene_name, "node.render_scene");
            let fluid = self.fluid_mesh(fluid_name);
            let object = self.node(format!("{scene_name}_fluid"), "node.scene_object");
            self.wire(fluid, "vertices", object, "vertices");
            self.wire(object, "object", scene, "object_0");
        }

        fn fluid_mesh(&mut self, fluid_name: &str) -> u32 {
            let domain = self.node(fluid_name, GPU_FLIP_DOMAIN_TYPE_ID);
            let mesh = self.node(format!("{fluid_name}_mesh"), "node.grid_mesh");
            self.wire(domain, "cell_size", mesh, "size_x");
            mesh
        }

        fn finish(self) -> EffectGraphDef {
            EffectGraphDef {
                version: 1,
                name: None,
                description: None,
                preset_metadata: None,
                scene_modifiers: Vec::new(),
                nodes: self.nodes,
                wires: self.wires,
            }
        }
    }

    fn prepare(
        builder: GraphBuilder,
    ) -> Result<Vec<CoupledSceneBinding>, SceneModifierExpandError> {
        prepare_coupled_scenes(&builder.finish(), &PrimitiveRegistry::with_builtin())
    }

    #[test]
    fn ordinary_bodies_and_copies_merge_into_one_binding() {
        let mut builder = GraphBuilder::new();
        builder.scene("scene", &[("fluid", "world", &[0], true)]);
        let result = prepare(builder).unwrap();
        assert_eq!(
            result,
            vec![CoupledSceneBinding {
                fluid: NodeId::new("fluid"),
                rigid: NodeId::new("world"),
                colliders: RigidImpulseTargets {
                    bodies: 1,
                    copies: true,
                },
            }]
        );
    }

    /// A matter domain pairs through its surface chain even though it takes no
    /// scene forces yet.
    #[test]
    fn matter_domain_pairs_through_its_surface_chain() {
        let mut builder = GraphBuilder::new();
        let scene = builder.node("scene", "node.render_scene");
        let world = builder.node("world", "node.physics_world");
        let body = builder.node("body", "node.rigid_body");
        builder.wire(body, "body", world, "body_0");
        let box_object = builder.node("box", "node.scene_object");
        builder.wire(world, "pose_0", box_object, "transform");
        builder.wire(box_object, "object", scene, "object_0");
        let domain = builder.node("matter", MATTER_DOMAIN_TYPE_ID);
        let state = builder.node("state", "node.matter_state");
        let mesh = builder.node("mesh", "node.volume_surface_mesh");
        let water = builder.node("water", "node.scene_object");
        builder.wire(domain, "ticks", state, "ticks");
        builder.wire(state, "out", mesh, "count");
        builder.wire(mesh, "vertices", water, "vertices");
        builder.wire(water, "object", scene, "object_1");
        assert_eq!(
            prepare(builder).unwrap(),
            vec![CoupledSceneBinding {
                fluid: NodeId::new("matter"),
                rigid: NodeId::new("world"),
                colliders: RigidImpulseTargets { bodies: 1, copies: false },
            }]
        );
    }

    #[test]
    fn material_parts_deduplicate_one_body_slot() {
        let mut builder = GraphBuilder::new();
        builder.scene("scene", &[("fluid", "world", &[0, 0], false)]);
        let result = prepare(builder).unwrap();
        assert_eq!(result[0].colliders.bodies, 1);
        assert!(!result[0].colliders.copies);
    }

    #[test]
    fn nested_group_keeps_scene_membership_and_stable_recipients() {
        let mut builder = GraphBuilder::new();
        let scene_id = builder.scene("scene", &[("fluid", "world", &[0], false)]);
        let selected: BTreeSet<_> = builder
            .nodes
            .iter()
            .filter(|node| node.id == scene_id || node.type_id == "node.scene_object")
            .map(|node| node.id)
            .collect();
        let (nodes, wires) = manifold_core::group_edit::group_selection(
            builder.nodes,
            builder.wires,
            &selected,
            "nested_scene",
            (0.0, 0.0),
        )
        .unwrap();
        let result = prepare(GraphBuilder {
            next_id: nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1,
            nodes,
            wires,
        })
        .unwrap();
        assert_eq!(result[0].fluid, NodeId::new("fluid"));
        assert_eq!(result[0].rigid, NodeId::new("world"));
    }

    #[test]
    fn singleton_and_render_only_scenes_are_inactive() {
        let mut builder = GraphBuilder::new();
        builder.rigid_only_scene("rigid_only", "rigid_world", &[0]);
        builder.fluid_only_scene("fluid_only", "fluid_domain");
        builder.render_only_scene("render_only");
        builder.node("dry", "node.render_scene");
        assert!(prepare(builder).unwrap().is_empty());
    }

    #[test]
    fn independent_scene_pairs_remain_independent_and_sorted() {
        let mut builder = GraphBuilder::new();
        builder.scene("z_scene", &[("z_fluid", "z_world", &[1], false)]);
        builder.scene("a_scene", &[("a_fluid", "a_world", &[0], false)]);
        let result = prepare(builder).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].fluid, NodeId::new("a_fluid"));
        assert_eq!(result[1].fluid, NodeId::new("z_fluid"));
    }

    #[test]
    fn repeated_pair_merges_body_masks_and_copies() {
        let mut builder = GraphBuilder::new();
        builder.scene("first", &[("fluid", "world", &[0], false)]);
        builder.scene("second", &[("fluid", "world", &[1], true)]);
        let result = prepare(builder).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].colliders.bodies, 0b11);
        assert!(result[0].colliders.copies);
    }

    #[test]
    fn multiple_fluid_domains_with_one_rigid_world_are_ambiguous() {
        let mut builder = GraphBuilder::new();
        builder.scene(
            "scene",
            &[
                ("fluid_a", "world", &[0], false),
                ("fluid_b", "world", &[1], false),
            ],
        );
        assert!(matches!(
            prepare(builder),
            Err(SceneModifierExpandError::AmbiguousScene { .. })
        ));
    }

    #[test]
    fn multiple_rigid_worlds_with_one_fluid_domain_are_ambiguous() {
        let mut builder = GraphBuilder::new();
        builder.scene(
            "scene",
            &[
                ("fluid", "world_a", &[0], false),
                ("fluid", "world_b", &[0], false),
            ],
        );
        assert!(matches!(
            prepare(builder),
            Err(SceneModifierExpandError::AmbiguousScene { .. })
        ));
    }

    #[test]
    fn one_fluid_domain_coupled_to_two_worlds_across_scenes_is_ambiguous() {
        let mut builder = GraphBuilder::new();
        builder.scene("first", &[("fluid", "world_a", &[0], false)]);
        builder.scene("second", &[("fluid", "world_b", &[0], false)]);
        assert!(matches!(
            prepare(builder),
            Err(SceneModifierExpandError::AmbiguousScene { .. })
        ));
    }

    #[test]
    fn one_rigid_world_coupled_to_two_domains_across_scenes_is_ambiguous() {
        let mut builder = GraphBuilder::new();
        builder.scene("first", &[("fluid_a", "world", &[0], false)]);
        builder.scene("second", &[("fluid_b", "world", &[0], false)]);
        assert!(matches!(
            prepare(builder),
            Err(SceneModifierExpandError::AmbiguousScene { .. })
        ));
    }

}
