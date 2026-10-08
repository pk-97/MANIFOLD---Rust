
    //! Regression tests for the LayerId-keyed chain/buf pools.
    //!
    //! The bug class these guard against: positional indexing
    //! (`Vec<EffectChain>` indexed by iteration counter or
    //! `layer_index`) caused chains to be re-bound to different
    //! layers when the active-clip set shifted or layers were
    //! reordered, forcing per-frame `PresetRuntime` rebuilds and
    //! wiping primitive state (Bloom mips, feedback buffers).
    //!
    //! These tests exercise the pool API directly. The structural
    //! invariant is: "same `LayerId` → same `EffectChain` map
    //! entry across frames, regardless of timeline position or
    //! iteration order." If that holds, every field of the
    //! `EffectChain` (including the cached `chain_graph`) survives
    //! by construction.
    use crate::layer_compositor::{CompositeClipDescriptor, LayerCompositor, CHAIN_GRACE_FRAMES};
    use crate::compositor::{CompositeLayerDescriptor, Compositor, CompositorFrame};
    use manifold_core::effects::EffectContainer;
    use manifold_core::{BlendMode, LayerId, PresetTypeId};
    use manifold_node_engine::runtime::PresetRuntime;
    use manifold_gpu::GpuTextureFormat;

    /// Build a minimal compositor. Tiny size keeps GPU costs low; tests
    /// don't render, so resolution doesn't matter.
    fn make_compositor() -> (manifold_gpu::testkit::TestDevice, LayerCompositor) {
        let device = manifold_gpu::testkit::test_device();
        let comp = LayerCompositor::new(&device, 64, 64);
        (device, comp)
    }

    /// Reserve capacity high enough that test insertions don't trigger
    /// an `AHashMap` rehash — preserves entry pointers for identity
    /// comparison.
    fn reserve_test_capacity(comp: &mut LayerCompositor) {
        comp.effect_chains.reserve(16);
        comp.chain_last_used_frame.reserve(16);
    }

    /// Build a minimal `CompositeLayerDescriptor` for tests that need to
    /// drive `trim_excess_buffers`. All defaults are inert (no clips,
    /// no effects, no group).
    fn make_layer_desc<'a>(
        layer_id: &'a LayerId,
        layer_index: i32,
    ) -> CompositeLayerDescriptor<'a> {
        CompositeLayerDescriptor {
            layer_index,
            layer_id,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            hidden: false,
            blit_to_led: false,
            layer_type: manifold_core::LayerType::Video,
            effects: &[],
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
        }
    }

    fn make_effect_layer(name: &str, enabled: bool, amount: f32) -> manifold_core::layer::Layer {
        let mut layer = manifold_core::layer::Layer::new(
            name.to_string(),
            manifold_core::LayerType::Video,
            0,
        );
        let mut fx = manifold_core::preset_definition_registry::create_default(
            &PresetTypeId::MIRROR,
        );
        fx.enabled = enabled;
        if let Some(param) = fx.params.iter_mut().next() {
            param.value = amount;
            param.base = amount;
        }
        layer.effects_mut().push(fx);
        layer
    }

    fn authored_layer_desc<'a>(
        layer: &'a manifold_core::layer::Layer,
    ) -> CompositeLayerDescriptor<'a> {
        CompositeLayerDescriptor {
            layer_index: 0,
            layer_id: &layer.layer_id,
            blend_mode: layer.default_blend_mode,
            opacity: layer.opacity,
            hidden: false,
            blit_to_led: layer.blit_to_led,
            layer_type: layer.layer_type,
            effects: layer.effects(),
            effect_groups: layer.effect_groups(),
            parent_layer_id: layer.parent_layer_id.as_ref(),
            is_group: layer.is_group(),
            trigger_count: 0,
        }
    }

    fn warm_effect_layer(
        comp: &mut LayerCompositor,
        device: &manifold_gpu::testkit::TestDevice,
        layer: &manifold_core::layer::Layer,
    ) {
        assert_eq!(
            comp.prewarm_layer_chains(
                layer,
                manifold_core::WarmupBudget::default(),
                device,
            ),
            manifold_core::WarmupOutcome::Quiescent,
            "minimal effect chain must quiesce during warmup",
        );
        assert!(comp
            .effect_chains
            .get(&layer.layer_id)
            .and_then(Option::as_ref)
            .is_some());
    }

    #[test]
    fn empty_authored_effects_drop_only_their_layer_chain() {
        let (device, mut comp) = make_compositor();
        let removed = make_effect_layer("removed", true, 1.0);
        let retained = make_effect_layer("retained", true, 1.0);
        warm_effect_layer(&mut comp, &device, &removed);
        warm_effect_layer(&mut comp, &device, &retained);

        let empty_effects = Vec::new();
        let removed_desc = CompositeLayerDescriptor {
            effects: &empty_effects,
            effect_groups: &[],
            ..authored_layer_desc(&removed)
        };
        let retained_desc = authored_layer_desc(&retained);
        comp.clear_obsolete_effect_chains(&[removed_desc, retained_desc], &[]);

        assert!(comp
            .effect_chains
            .get(&removed.layer_id)
            .is_some_and(Option::is_none));
        assert!(comp.chain_last_used_frame.contains_key(&removed.layer_id));
        assert!(comp
            .effect_chains
            .get(&retained.layer_id)
            .and_then(Option::as_ref)
            .is_some());
        assert!(comp.chain_last_used_frame.contains_key(&retained.layer_id));
    }

    #[test]
    fn disabled_and_zero_amount_effects_retain_their_chain() {
        let (device, mut comp) = make_compositor();
        let mut layer = make_effect_layer("retained", true, 1.0);
        warm_effect_layer(&mut comp, &device, &layer);

        layer.effects_mut()[0].enabled = false;
        let disabled_desc = authored_layer_desc(&layer);
        comp.clear_obsolete_effect_chains(&[disabled_desc], &[]);
        assert!(comp
            .effect_chains
            .get(&layer.layer_id)
            .and_then(Option::as_ref)
            .is_some());

        layer.effects_mut()[0].enabled = true;
        layer.effects_mut()[0]
            .params
            .iter_mut()
            .next()
            .expect("minimal effect has an amount parameter")
            .value = 0.0;
        let zero_amount_desc = authored_layer_desc(&layer);
        comp.clear_obsolete_effect_chains(&[zero_amount_desc], &[]);
        assert!(comp
            .effect_chains
            .get(&layer.layer_id)
            .and_then(Option::as_ref)
            .is_some());
    }

    #[test]
    fn empty_frame_master_effects_drop_master_chain_before_early_return() {
        let (device, mut comp) = make_compositor();
        let mut project = manifold_core::project::Project::default();
        project.settings.master_effects.push(
            manifold_core::preset_definition_registry::create_default(&PresetTypeId::MIRROR),
        );
        assert_eq!(
            comp.prewarm_master_chain(
                &project,
                manifold_core::WarmupBudget::default(),
                &device,
                None,
                (1, 1),
            ),
            manifold_core::WarmupOutcome::Quiescent,
            "minimal master chain must quiesce during warmup",
        );
        assert!(comp.master_effect_chain.is_some());

        let frame = CompositorFrame {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            project_tempo: None,
            frame_count: 0,
            compositor_dirty: true,
            clips: &[],
            layers: &[],
            master_effects: &[],
            master_effect_groups: &[],
            master_trigger_count: 0,
            tonemap: crate::tonemap::TonemapSettings::default(),
            led_exit_index: -1,
            led_composite_size: (1, 1),
            output_width: 64,
            output_height: 64,
            occluded_layers: &[],
            render_skip: &[],
        };
        let mut enc = device.create_encoder("empty-frame-obsolete-chain");
        let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut enc, &device);
        let _ = comp.render(&mut gpu, &frame);
        enc.commit_and_wait_completed();

        assert!(comp.master_effect_chain.is_none());
        assert!(comp.led_master_ec.is_none());
    }

    #[test]
    fn chain_entry_stable_across_active_set_changes() {
        // Mirrors the live bug: active layer set shifts frame-to-frame
        // (clips firing/stopping). Each LayerId's chain entry must
        // survive intact regardless of which other layers are active.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let a = LayerId::from("A");
        let b = LayerId::from("B");
        let c = LayerId::from("C");

        // Frame 1: A + B active.
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&a);
        comp.ensure_chain_for_layer(&b);
        let a_ptr = comp.effect_chains.get(&a).unwrap() as *const Option<PresetRuntime>;
        let b_ptr = comp.effect_chains.get(&b).unwrap() as *const Option<PresetRuntime>;

        // Frame 2: B + C active (A goes quiet, C new).
        comp.frame_counter = 2;
        comp.ensure_chain_for_layer(&b);
        comp.ensure_chain_for_layer(&c);

        // B is the same instance — its chain_graph, primitive state,
        // and all internal buffers are preserved.
        assert_eq!(
            comp.effect_chains.get(&b).unwrap() as *const Option<PresetRuntime>,
            b_ptr,
            "B's chain instance must be identical across frame transition",
        );
        // A is still in the pool (within grace period).
        assert_eq!(
            comp.effect_chains.get(&a).unwrap() as *const Option<PresetRuntime>,
            a_ptr,
            "A's chain instance must persist within CHAIN_GRACE_FRAMES",
        );

        // Frame 3: A + B + C all active again.
        comp.frame_counter = 3;
        comp.ensure_chain_for_layer(&a);
        comp.ensure_chain_for_layer(&b);
        comp.ensure_chain_for_layer(&c);

        // All entries still the same instances.
        assert_eq!(comp.effect_chains.get(&a).unwrap() as *const _, a_ptr);
        assert_eq!(comp.effect_chains.get(&b).unwrap() as *const _, b_ptr);
    }

    #[test]
    fn chain_entry_independent_of_layer_index() {
        // The original Vec<EffectChain> indexed by `layer_index as usize`
        // would have bound chains to timeline positions; dragging a layer
        // up/down the timeline would have shuffled which chain each layer
        // received. LayerId keying makes that impossible: only the id
        // matters, regardless of `layer_index`.
        //
        // We can't reorder layers without going through the full render
        // pipeline, but we can prove the structural invariant directly:
        // ensure_chain_for_layer takes a LayerId, never a layer_index.
        // The map is `AHashMap<LayerId, EffectChain>` — `chains[5]`
        // (a `usize` index) doesn't compile.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let x = LayerId::from("X");
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&x);
        let x_ptr = comp.effect_chains.get(&x).unwrap() as *const Option<PresetRuntime>;

        // Simulate many frames of reorder activity: ensure many other
        // layers come/go but X stays present.
        for f in 2..20 {
            comp.frame_counter = f;
            // "Other layers at varying timeline positions" — irrelevant
            // because keying is by LayerId, not position.
            let other = LayerId::from(format!("other-{f}"));
            comp.ensure_chain_for_layer(&other);
            comp.ensure_chain_for_layer(&x);
        }

        // X's chain is still the same instance.
        assert_eq!(
            comp.effect_chains.get(&x).unwrap() as *const Option<PresetRuntime>,
            x_ptr,
            "X's chain instance must survive arbitrary other-layer churn",
        );
    }

    #[test]
    fn master_chain_is_separate_field_from_layer_chains() {
        // The master FX pass operates on the composited scene — it has
        // no `LayerId` to key by, so it lives in a dedicated field.
        // This makes "master chain accidentally bound to layer N's chain"
        // structurally impossible: different types, different fields.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let any_layer = LayerId::from("any");
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&any_layer);

        let layer_chain_ptr = comp.effect_chains.get(&any_layer).unwrap() as *const Option<PresetRuntime>;
        let master_chain_ptr: *const Option<PresetRuntime> = &comp.master_effect_chain;

        assert_ne!(
            layer_chain_ptr, master_chain_ptr,
            "master_effect_chain must be a different instance from any layer chain",
        );
    }

    #[test]
    fn chain_dropped_immediately_when_layer_removed_from_project() {
        // Event-based eviction: when a layer disappears from
        // `frame.layers` (project edit removed it), its chain drops on
        // the next `trim_excess_buffers` call — no waiting for the
        // grace timer. This bounds memory tightly to the project's
        // current layer set.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let kept = LayerId::from("kept");
        let removed = LayerId::from("removed");

        // Frame 1: both layers exist and touch their chains.
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&kept);
        comp.ensure_chain_for_layer(&removed);
        assert!(comp.effect_chains.contains_key(&kept));
        assert!(comp.effect_chains.contains_key(&removed));

        // Frame 2: the user deletes `removed` from the project. The next
        // CompositorFrame includes only `kept` in its `layers` slice.
        // Even though `removed`'s chain was just touched, trim drops it
        // immediately — the layer no longer exists.
        comp.frame_counter = 2;
        let layers_after_delete = vec![make_layer_desc(&kept, 0)];
        comp.trim_excess_buffers(&layers_after_delete);

        assert!(
            !comp.effect_chains.contains_key(&removed),
            "chain for a deleted layer must drop on the next trim, not wait for grace",
        );
        assert!(
            comp.effect_chains.contains_key(&kept),
            "chain for a layer still in the project must survive",
        );
    }

    #[test]
    fn aged_chains_pruned_after_grace_period() {
        // Timer-based safety net: a chain whose layer is still in the
        // project but hasn't been touched in CHAIN_GRACE_FRAMES is
        // dropped. Catches the "operator moved on from this section
        // hours ago" case in long live shows.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let stale = LayerId::from("stale");
        let alive = LayerId::from("alive");

        // Frame 1: both active and touch their chains.
        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&stale);
        comp.ensure_chain_for_layer(&alive);

        // Advance well past the grace window while only refreshing `alive`.
        // BOTH layers stay in `frame.layers` — only `stale`'s chain is idle.
        let last_frame = CHAIN_GRACE_FRAMES + 50;
        for f in 2..=last_frame {
            comp.frame_counter = f;
            comp.ensure_chain_for_layer(&alive);
        }

        let layers = vec![make_layer_desc(&stale, 0), make_layer_desc(&alive, 1)];
        comp.trim_excess_buffers(&layers);

        assert!(
            !comp.effect_chains.contains_key(&stale),
            "stale chain must have been pruned after exceeding CHAIN_GRACE_FRAMES",
        );
        assert!(
            comp.effect_chains.contains_key(&alive),
            "alive chain must still be present",
        );
    }

    #[test]
    fn chain_survives_layer_idle_within_grace() {
        // Common live-performance case: a layer mutes / has no active
        // clip for a short window (typical mid-song breakdown), then
        // resumes. Its chain — and any feedback state it holds — must
        // survive the gap so the visual look is continuous.
        let (_device, mut comp) = make_compositor();
        reserve_test_capacity(&mut comp);

        let idle = LayerId::from("idle");

        comp.frame_counter = 1;
        comp.ensure_chain_for_layer(&idle);
        let initial_ptr = comp.effect_chains.get(&idle).unwrap() as *const Option<PresetRuntime>;

        // Many frames pass without `idle` being touched, but the layer
        // is still in the project (typical mute / clip-gap scenario).
        // CHAIN_GRACE_FRAMES is 18000 — pick a value well below it.
        let layers = vec![make_layer_desc(&idle, 0)];
        for f in 2..=(CHAIN_GRACE_FRAMES / 4) {
            comp.frame_counter = f;
            comp.trim_excess_buffers(&layers);
        }

        assert_eq!(
            comp.effect_chains.get(&idle).unwrap() as *const _,
            initial_ptr,
            "chain instance must survive layer-idle periods well below grace window",
        );
    }

    /// P2 chain warmup: a layer with enabled post-fx effects must have its
    /// `PresetRuntime` built at load time, and the first playback frames must
    /// not record any chain-construction cold touches.
    #[test]
    fn warmup_builds_layer_post_fx_chain_and_zero_cold_touches_on_play() {
        use crate::compositor::{Compositor, CompositorFrame};
        use manifold_node_engine::gpu::render_target::RenderTarget;
        use manifold_core::effect_graph_def::ParamSpecDef;
        use manifold_core::effects::PresetInstance;
        use manifold_core::layer::Layer;
        use manifold_core::params::{Param, ParamManifest};
        use manifold_core::types::LayerType;
        use manifold_foundation::cold_touch::{
            reset_cold_touch_counts, set_transport_playing, total_cold_touches,
        };

        fn slot(id: &str, value: f32) -> Param {
            let mut p = Param::bundled(ParamSpecDef {
                tooltip: None,
                id: id.into(),
                name: id.into(),
                min: 0.0,
                max: 1.0,
                default_value: value,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: vec![],
                format_string: None,
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
                card_visible: true,
                material_role: None,
            });
            p.value = value;
            p.base = value;
            p.exposed = true;
            p
        }

        let (device, mut comp) = make_compositor();
        let mut layer = Layer::new("fx-layer".to_string(), LayerType::Video, 0);
        let mut fx = PresetInstance::new(manifold_core::PresetTypeId::new("Invert"));
        fx.params = ParamManifest::from_params(vec![slot("amount", 1.0)]);
        layer.effects_mut().push(fx);

        // Warmup should build the per-layer chain.
        let outcome = comp.prewarm_layer_chains(
            &layer,
            manifold_core::WarmupBudget::default(),
            &device,
        );
        assert_eq!(
            outcome,
            manifold_core::WarmupOutcome::Quiescent,
            "Invert chain must quiesce within default budget"
        );
        let chain = comp
            .effect_chains
            .get(&layer.layer_id)
            .expect("chain slot must exist after warmup")
            .as_ref()
            .expect("chain runtime must be built after warmup");
        assert!(
            !chain.warmup_pending(),
            "warmed chain must report no pending work"
        );

        // Simulate a first playback frame: one clip on the layer, with layer FX.
        let clip_tex = RenderTarget::new(
            &device,
            64,
            64,
            GpuTextureFormat::Rgba16Float,
            "warmup test clip",
        );
        let clip = CompositeClipDescriptor {
            clip_id: "clip-1",
            texture: &clip_tex.texture,
            layer_index: layer.index,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: false,
            effects: &[],
            effect_groups: &[],
        };
        let layer_desc = CompositeLayerDescriptor {
            layer_index: layer.index,
            layer_id: &layer.layer_id,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            hidden: false,
            blit_to_led: false,
            layer_type: manifold_core::LayerType::Video,
            effects: layer.effects(),
            effect_groups: layer.effect_groups(),
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
        };
        let frame = CompositorFrame {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            project_tempo: None,
            frame_count: 0,
            compositor_dirty: true,
            clips: std::slice::from_ref(&clip),
            layers: std::slice::from_ref(&layer_desc),
            master_effects: &[],
            master_effect_groups: &[],
            master_trigger_count: 0,
            tonemap: crate::tonemap::TonemapSettings::default(),
            led_exit_index: -1,
            led_composite_size: (1, 1),
            output_width: 64,
            output_height: 64,
            occluded_layers: &[],
            render_skip: &[],
        };

        // One render to ensure the cached chain is exercised, then reset and
        // sample the cold-touch counter over 60 frames.
        let mut enc = device.create_encoder("warmup play");
        let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut enc, &device);
        let _ = comp.render(&mut gpu, &frame);
        enc.commit_and_wait_completed();

        reset_cold_touch_counts();
        set_transport_playing(true);
        for f in 0..60 {
            let mut enc = device.create_encoder("warmup play");
            let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut enc, &device);
            let _ = comp.render(&mut gpu, &frame);
            enc.commit_and_wait_completed();
            // Silence unused warning in release builds.
            let _ = f;
        }
        assert_eq!(
            total_cold_touches(),
            0,
            "no chain construction (or other first-touch work) during playback after warmup"
        );
        set_transport_playing(false);
    }
