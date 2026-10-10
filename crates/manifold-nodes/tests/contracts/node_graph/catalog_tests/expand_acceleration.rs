use manifold_core::NodeId;

    use manifold_node_engine::load::expand::testkit::{recipient_key, impulse_recipients_with_index};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::liquid_domain_of;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_core::scene_impulse::ImpulseTarget;

    fn preset(json: &str) -> EffectGraphDef {
        serde_json::from_str(json).expect("preset parses")
    }

    fn top(node: &str) -> SceneNodeRef {
        SceneNodeRef { scope: Vec::new(), node: NodeId::new(node) }
    }

    /// A matter domain is found by the same walk, through its group, and
    /// takes scene forces and impulses on the same port FLIP does.
    #[test]
    fn matter_surface_water_resolves_to_its_domain_force_port() {
        let def = preset(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterDamBreakMatter.json")));
        let registry = PrimitiveRegistry::with_builtin();
        let index = FlatSceneIndex::build(&def).unwrap();
        let water = top("water_object");
        let domain = liquid_domain_of(&index, &water).unwrap().expect("matter water has a domain");
        assert_eq!(domain.node, NodeId::new("matter_domain"));
        assert_eq!(
            recipient_key(&index, &water, &registry).unwrap(),
            Some((domain, "acceleration_field".to_string()))
        );
        let recipients = impulse_recipients_with_index(
            &index,
            &top("scene"),
            &SceneTargetSelection::Explicit { objects: vec![water] },
            &registry,
        )
        .unwrap();
        assert_eq!(recipients, vec![(NodeId::new("matter_domain"), ImpulseTarget::Fluid)]);
    }
