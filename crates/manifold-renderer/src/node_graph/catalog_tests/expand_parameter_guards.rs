use manifold_node_engine::load::expand::testkit::GuardFixture as PreparedModifierParameterGuards;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::NodeId;
    use manifold_node_engine::persistence::EffectGraphDefExt;

    const LEGACY_SURFACE_PEEL: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/scene-modifiers/surface_peel_applied_v2.json"
    ));

    #[test]
    fn legacy_fragment_accepts_rt_toggles_and_validates_target() {
        let owner: EffectGraphDef =
            serde_json::from_str(LEGACY_SURFACE_PEEL).expect("legacy fixture parses");
        let guards = PreparedModifierParameterGuards::prepare(&owner)
            .expect("legacy fragment graph should prepare");
        assert_eq!(
            guards.scenes(),
            vec![NodeId::new("scan_render")],
            "the legacy downstream render scene remains a valid target"
        );
        assert!(guards.sources_empty());

        let mut graph = owner
            .into_graph(
                &manifold_node_engine::persistence::PrimitiveRegistry::with_builtin(),
                &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default(),
            )
            .expect("legacy fixture graph builds");
        let scene = graph
            .instance_by_node_id(&NodeId::new("scan_render"))
            .expect("legacy render scene");
        graph.set_param_unchecked(
            scene,
            "rt_enabled",
            manifold_node_engine::parameters::ParamValue::Bool(true),
        );
        guards
            .install(&mut graph)
            .expect("RT-enabled legacy scene remains admissible");
        assert!(graph
            .set_param(scene, "rt_enabled", manifold_node_engine::parameters::ParamValue::Bool(false))
            .is_ok());
        assert!(graph
            .set_param(scene, "rt_enabled", manifold_node_engine::parameters::ParamValue::Bool(true))
            .is_ok());
    }
