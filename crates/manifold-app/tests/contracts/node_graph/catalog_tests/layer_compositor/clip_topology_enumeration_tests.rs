
    //! P7 D17 (WARMUP_DESIGN section 5) value-level coverage: the per-clip
    //! topology enumeration dedups by the production topology hash without
    //! touching a GPU. Two clips with identical effective post-fx sets must
    //! yield one topology; differing sets must yield two.
    use manifold_compositor::layer_compositor::unique_clip_chain_topologies;
    use manifold_core::effects::PresetInstance;
    use manifold_core::PresetTypeId;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;

    fn make_fx(ty: PresetTypeId) -> PresetInstance {
        let mut fx = manifold_core::preset_definition_registry::create_default(&ty);
        // `has_enabled_effects` gates on the first param being > 0 — the
        // registry default for these presets is 1.0, but pin it so the test
        // doesn't depend on preset defaults.
        if let Some(p) = fx.params.iter_mut().next() {
            p.value = 1.0;
        }
        fx
    }

    fn make_layer() -> manifold_core::layer::Layer {
        Layer::new("gen".to_string(), LayerType::Generator, 0)
    }

    #[test]
    fn clips_with_identical_post_fx_sets_produce_one_topology() {
        let mut layer = make_layer();
        layer.effects_mut().push(make_fx(PresetTypeId::MIRROR));
        for _ in 0..3 {
            layer.clips.push(TimelineClip::default());
        }

        let topos = unique_clip_chain_topologies(std::slice::from_ref(&layer), 256, 256);
        assert_eq!(
            topos.len(),
            1,
            "three clips with the same (empty) clip post-fx must collapse to \
             the layer's single topology",
        );
        assert_eq!(
            topos[0].effects.len(),
            1,
            "the collapsed topology carries the layer's effective post-fx set",
        );
    }

    #[test]
    fn clips_with_differing_post_fx_sets_produce_distinct_topologies() {
        let mut layer = make_layer();
        layer.effects_mut().push(make_fx(PresetTypeId::MIRROR));
        let mut clip_a = TimelineClip::default();
        clip_a.effects.push(make_fx(PresetTypeId::COLOR_GRADE));
        layer.clips.push(clip_a);
        let mut clip_b = TimelineClip::default();
        clip_b.effects.push(make_fx(PresetTypeId::VORONOI_PRISM));
        layer.clips.push(clip_b);
        layer.clips.push(TimelineClip::default());

        let topos = unique_clip_chain_topologies(std::slice::from_ref(&layer), 256, 256);
        assert_eq!(
            topos.len(),
            3,
            "layer topology + two distinct clip post-fx sets must yield \
             three unique topologies (one per distinct set)",
        );
    }

    #[test]
    fn same_effect_shape_different_identity_stays_distinct() {
        // The production hash keys on per-instance effect ids, not effect
        // types — two same-typed effects with different ids are different
        // topologies, and a shape-only dedup would silently pool them.
        let mut layer = make_layer();
        let mut clip_a = TimelineClip::default();
        clip_a.effects.push(make_fx(PresetTypeId::MIRROR));
        let mut clip_b = TimelineClip::default();
        clip_b.effects.push(make_fx(PresetTypeId::MIRROR));
        layer.clips.push(clip_a);
        layer.clips.push(clip_b);

        let topos = unique_clip_chain_topologies(std::slice::from_ref(&layer), 256, 256);
        assert_eq!(
            topos.len(),
            2,
            "same effect type with a different instance id is a distinct \
             topology — the production key is id-keyed",
        );
    }
