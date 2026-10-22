use manifold_core::NodeId;
use manifold_node_engine::load::expand::{SceneModifierExpandError, SceneModifierNodeRoute};
use manifold_node_engine::load::expand::testkit::prepare_impulses as prepare;

    use manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
    use manifold_node_engine::load::expand::{SceneModifierNodeCopy, prepare_scene_modifiers};
    use manifold_node_engine::{graph::Graph, persistence::PrimitiveRegistry};
    use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
    use manifold_nodes_scene::node_graph::scene_modifier_authoring::attach_scene_modifier;
    use manifold_core::scene_modifier_preset::SceneTargetSelection;

    const PHYSICS_SOLIDS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/presets/PhysicsSolids.json"
    ));
    const UNIFORM_FORCE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/UniformForce.json"
    ));

    fn stock_uniform_force() -> EffectGraphDef {
        let host: EffectGraphDef =
            serde_json::from_str(PHYSICS_SOLIDS).expect("PhysicsSolids fixture parses");
        let recipe: EffectGraphDef =
            serde_json::from_str(UNIFORM_FORCE).expect("UniformForce fixture parses");
        let scene = NodeId::new("scene");
        let instance = prepare_new_scene_modifier(
            &host,
            &recipe,
            NodeId::new("route_uniform"),
            scene,
            SceneTargetSelection::Explicit {
                objects: vec![NodeId::new("physics_demo_114")],
            },
        )
        .expect("stock force attaches");
        attach_scene_modifier(&host, instance)
            .expect("stock force inserts")
    }

    fn append_host_binding(owner: &mut EffectGraphDef, id: &str, param_id: &str, is_trigger: bool) {
        let metadata = owner
            .preset_metadata
            .as_mut()
            .expect("PhysicsSolids metadata");
        metadata.params.push(
            serde_json::from_value(serde_json::json!({
                "id": id,
                "name": id,
                "min": 0.0,
                "max": 1.0,
                "defaultValue": 0.0,
                "isTrigger": is_trigger
            }))
            .expect("host param parses"),
        );
        metadata.bindings.push(
            serde_json::from_value(serde_json::json!({
                "id": id,
                "label": id,
                "defaultValue": 0.0,
                "target": {
                    "kind": "sceneModifier",
                    "modifierId": "route_uniform",
                    "paramId": param_id
                }
            }))
            .expect("host binding parses"),
        );
    }

    fn generated_field_graph(type_id: &str) -> Graph {
        let registry = PrimitiveRegistry::with_builtin();
        let mut graph = Graph::new();
        let node = graph.add_node(
            registry
                .construct(type_id)
                .unwrap_or_else(|| panic!("primitive {type_id} is registered")),
        );
        graph.set_node_id(node, NodeId::new("generated_field"));
        graph
    }

    fn manual_route_fixture(
        field: NodeId,
        port: &str,
        object: Option<NodeId>,
        type_id: &str,
    ) -> (EffectGraphDef, Vec<SceneModifierNodeRoute>, Graph) {
        let mut owner = stock_uniform_force();
        owner.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .expect("force metadata")
            .scene_modifier
            .as_mut()
            .expect("force recipe")
            .impulses[0]
            .field = field.clone();
        owner.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .expect("force metadata")
            .scene_modifier
            .as_mut()
            .expect("force recipe")
            .impulses[0]
            .port = port.into();
        let routes = vec![SceneModifierNodeRoute {
            modifier_id: NodeId::new("route_uniform"),
            local: NodeId::new("force_impulse_gate"),
            copies: vec![SceneModifierNodeCopy {
                object,
                node_id: NodeId::new("generated_field"),
            }],
        }];
        (owner, routes, generated_field_graph(type_id))
    }

    #[test]
    fn scene_impulse_route_from_uniform_force_is_stable_and_retains_identity_alias() {
        let mut owner = stock_uniform_force();
        append_host_binding(&mut owner, "fire_alias", "fire", true);
        append_host_binding(&mut owner, "direction_alias", "direction_x", false);

        let prepared = prepare_scene_modifiers(&owner, &PrimitiveRegistry::with_builtin())
            .expect("stock force prepares");
        assert_eq!(prepared.impulse_routes.len(), 1);
        let route = &prepared.impulse_routes[0];
        assert_eq!(route.modifier_id, NodeId::new("route_uniform"));
        assert_eq!(route.param_id, "fire");
        assert_eq!(route.field_port, "out");
        assert!(!route.field_node.is_empty());

        let metadata = prepared
            .def
            .preset_metadata
            .as_ref()
            .expect("host metadata");
        assert!(metadata.params.iter().any(|param| param.id == "fire_alias" && param.is_trigger));
        assert!(!metadata.bindings.iter().any(|binding| binding.id == "fire_alias"));
        let ordinary = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == "direction_alias")
            .expect("ordinary alias expanded");
        assert!(matches!(ordinary.target, BindingTarget::Node { .. }));

        let serialized = serde_json::to_string(&owner).expect("authored graph serializes");
        let round_trip: EffectGraphDef =
            serde_json::from_str(&serialized).expect("prepared graph reparses");
        let prepared_again =
            prepare_scene_modifiers(&round_trip, &PrimitiveRegistry::with_builtin())
                .expect("round-tripped force prepares");
        assert_eq!(prepared.impulse_routes, prepared_again.impulse_routes);
    }

    #[test]
    fn scene_impulse_route_rejects_missing_local_leaf() {
        let missing_leaf = NodeId::new("missing_field");
        let (owner, routes, mut graph) =
            manual_route_fixture(missing_leaf, "out", None, "node.uniform_vector_field");
        assert!(matches!(
            prepare(&owner, &routes, &mut graph),
            Err(SceneModifierExpandError::MissingTarget { path, .. })
                if path.ends_with(".field")
        ));
    }

    #[test]
    fn scene_impulse_route_rejects_missing_port_and_wrong_type() {
        let field = NodeId::new("force_impulse_gate");
        let (owner, routes, mut graph) =
            manual_route_fixture(field.clone(), "missing", None, "node.uniform_vector_field");
        assert!(matches!(
            prepare(&owner, &routes, &mut graph),
            Err(SceneModifierExpandError::MissingTarget { path, .. })
                if path.ends_with(".port")
        ));

        let (owner, routes, mut graph) = manual_route_fixture(field, "out", None, "node.value");
        assert!(matches!(
            prepare(&owner, &routes, &mut graph),
            Err(SceneModifierExpandError::InvalidRecipe { path, detail })
                if path.ends_with(".port") && detail.contains("VectorField")
        ));
    }

    #[test]
    fn scene_impulse_route_rejects_each_object_source() {
        let field = NodeId::new("force_impulse_gate");
        let (owner, routes, mut graph) = manual_route_fixture(
            field,
            "out",
            Some(NodeId::new("physics_demo_114")),
            "node.uniform_vector_field",
        );
        assert!(matches!(
            prepare(&owner, &routes, &mut graph),
            Err(SceneModifierExpandError::InvalidRecipe { path, detail })
                if path.ends_with(".field") && detail.contains("eachObject")
        ));
    }
