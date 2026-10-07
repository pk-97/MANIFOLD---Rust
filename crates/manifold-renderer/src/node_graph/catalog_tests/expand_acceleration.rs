use manifold_core::NodeId;

    use crate::node_graph::scene_modifier_expand::testkit::{authoring_objects, recipient_key, impulse_recipients_with_index};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::liquid_domain_of;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use crate::node_graph::persistence::PrimitiveRegistry;
    use crate::node_graph::physics_events::ImpulseTarget;

    fn preset(json: &str) -> EffectGraphDef {
        serde_json::from_str(json).expect("preset parses")
    }

    fn top(node: &str) -> SceneNodeRef {
        SceneNodeRef { scope: Vec::new(), node: NodeId::new(node) }
    }

    /// BUG-4lfm (GPU-surface water not recognised as water): the water object
    /// is fed by particles_b → sort → blobs → volume → marching cubes, never
    /// by fluid_surface.vertices, and must still reach its domain.
    #[test]
    fn gpu_surface_water_resolves_to_its_flip_domain() {
        let def = preset(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/generator-presets/WaterDamBreakGpu.json")));
        let registry = PrimitiveRegistry::with_builtin();
        let index = FlatSceneIndex::build(&def).unwrap();
        let water = top("water_object");
        assert_eq!(liquid_domain_of(&index, &water).unwrap(), Some(top("fluid_surface")));
        assert_eq!(
            recipient_key(&index, &water, &registry).unwrap(),
            Some((top("fluid_surface"), "acceleration_field".to_string()))
        );
        assert!(authoring_objects(&def, &top("scene"), &registry).unwrap().contains(&water));
        let recipients = impulse_recipients_with_index(
            &index,
            &top("scene"),
            &SceneTargetSelection::Explicit { objects: vec![water] },
            &registry,
        )
        .unwrap();
        assert_eq!(recipients, vec![(NodeId::new("fluid_surface"), ImpulseTarget::Fluid)]);
        for rigid in ["floor_object", "obstacle_object"] {
            assert_eq!(liquid_domain_of(&index, &top(rigid)).unwrap(), None, "{rigid}");
        }
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
