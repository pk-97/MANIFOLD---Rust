    /// The gate only saves work if the preset actually wires a trigger into it.
    /// FluidSim2D routes the clip-trigger counter into seed_spawn.reset_trigger,
    /// so the four-pass compaction runs only on a reset edge, not every frame. Guards
    /// against the wire being dropped (which would silently revert to per-frame work).
    #[test]
    fn fluidsim_wires_a_trigger_into_the_seed_reset_trigger() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        let json = crate::bundled_presets::bundled_preset_json(
            &manifold_core::PresetTypeId::new("FluidSim2D"),
        )
        .expect("FluidSim2D bundled");
        let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let flat = manifold_core::flatten::flatten_groups(&def).expect("FluidSim2D flattens");
        let seed = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.spawn_from_image")
            .expect("FluidSim has a seed node");
        let wired = flat
            .wires
            .iter()
            .any(|w| w.to_node == seed.id && w.to_port == "reset_trigger");
        assert!(
            wired,
            "FluidSim must wire a trigger into the seed's reset_trigger, else the seed \
             recomputes its 4-pass compaction every frame instead of only on reset"
        );
    }
