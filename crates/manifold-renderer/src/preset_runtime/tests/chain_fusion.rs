    //! Cross-card chain fusion integration (docs/CHAIN_FUSION_DESIGN.md):
    //! the per-card build and the fused-segment build of the SAME two-card
    //! chain must render within the pointwise fusion budget of each other,
    //! and the cards' `param_values` must keep driving the fused chain
    //! through the retargeted bindings.

    use super::*;
    use crate::gpu_encoder::GpuEncoder;
    use crate::node_graph::freeze::TextureDiff;
    use crate::node_graph::freeze::install as freeze_install;
    use crate::preset_context::PresetContext;
    use half::f16;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use manifold_gpu::{
        GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    fn set_param(fx: &mut PresetInstance, id: &str, v: f32) {
        let ty = fx.effect_type().clone();
        let p = fx
            .params
            .get_mut(id)
            .unwrap_or_else(|| panic!("param id `{id}` on {ty:?}"));
        p.value = v;
        p.base = v;
    }

    fn ctx(w: u32, h: u32) -> PresetContext {
        PresetContext {
            time: 0.5,
            beat: 1.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        }
    }

    fn gradient_input(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / w as f32);
                px[i + 1] = f16::from_f32(y as f32 / h as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "chain-fusion-test-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn run_once(
        cg: &mut PresetRuntime,
        device: &manifold_gpu::GpuDevice,
        input: &manifold_gpu::GpuTexture,
        effects: &[PresetInstance],
        pc: &PresetContext,
    ) {
        let mut enc = device.create_encoder("chain-fusion-test");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            cg.run(&mut gpu, input, effects, &[], pc);
        }
        enc.commit_and_wait_completed();
    }

    /// Copy a runtime's current output into a standalone target so a later
    /// run can't overwrite it.
    fn snapshot_output(
        cg: &PresetRuntime,
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> crate::render_target::RenderTarget {
        let out = cg.output_texture().expect("chain produced output");
        let rt = crate::render_target::RenderTarget::new(
            device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            "chain-fusion-test-snap",
        );
        let mut enc = device.create_encoder("chain-fusion-snap");
        enc.copy_texture_to_texture(out, &rt.texture, w, h, 1);
        enc.commit_and_wait_completed();
        rt
    }

    #[test]
    fn fused_segment_build_matches_per_card_build_and_stays_param_driven() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // Two adjacent ColorGrades with distinct, non-trivial params — the
        // same type twice exercises the segment namespacing on real presets.
        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        set_param(&mut e1, "gain", 1.2);
        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        set_param(&mut e2, "gain", 0.85);
        set_param(&mut e2, "saturation", 0.6);
        let effects = vec![e1, e2];

        // ── Per-card build first: the segment cache is cold, the lookup goes
        // Pending (tests never enqueue the worker), and the chain splices
        // per-card — today's production path, our oracle. ──
        let mut per_card = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("per-card chain builds");
        assert_eq!(per_card.effect_nodes.len(), 2);
        assert!(
            per_card.pending_segments,
            "cold cache must leave the chain waiting on the segment compile"
        );
        assert!(
            !per_card.awaiting_segment_swap(),
            "no swap signal until a worker result lands"
        );

        // ── Compile the segment synchronously (the worker's job, minus the
        // gate) and seed the cache, then rebuild: the Ready path. ──
        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        freeze_install::seed_segment_cache_for_test(&cards, &primitives)
            .expect("two pointwise ColorGrades fuse across the seam");

        let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused-segment chain builds");
        assert_eq!(fused.effect_nodes.len(), 2, "one EffectSlot per card survives");
        assert!(!fused.pending_segments);
        let fused_kernels = fused
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        assert_eq!(
            fused_kernels, 1,
            "both cards must collapse into ONE cross-card kernel"
        );

        // ── Parity at build params. ──
        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment must match per-card chain: max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );

        // ── Live param drive: move card 2's gain on the host slice only. The
        // binding apply path must push it into the fused kernel's uniform. ──
        let before = snapshot_output(&fused, &device, w, h);
        let mut effects2 = effects.clone();
        set_param(&mut effects2[1], "gain", 1.6);
        run_once(&mut fused, &device, &input, &effects2, &pc);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "a card slider move must visibly drive the fused segment"
        );
        run_once(&mut per_card, &device, &input, &effects2, &pc);
        let r2 = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r2.passes(0.005) && r2.over_count < 64,
            "after the slider move the two builds must still agree: max_abs={}, over={}/{}",
            r2.max_abs,
            r2.over_count,
            r2.total
        );
    }

    /// BUG-111: an in-place inner-param edit (value/position edit — bumps
    /// `graph_version` only, no rebuild) on a card that is a member of a
    /// fused multi-card SEGMENT must still reach the live kernel. The
    /// segment's `node_map`/`fused_retarget` are keyed with the `c{i}.`
    /// per-card prefix (`freeze::segment::card_prefix`), built from the
    /// concatenated segment def, while the per-frame override path reads
    /// each card's own UNPREFIXED `fx.graph`. Without translating through
    /// that prefix (`EffectSlot::card_prefix` →
    /// `BoundGraph::apply_inner_overrides_prefixed`) the override misses
    /// every node in the map and silently no-ops — the old value keeps
    /// rendering until an unrelated rebuild. Segment sibling of
    /// `bound_graph::inner_override_routes_fused_away_node_through_retarget`.
    #[test]
    fn fused_segment_inner_override_reaches_live_kernel() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // Two adjacent ColorGrades — same fusable two-card segment shape as
        // `fused_segment_build_matches_per_card_build_and_stays_param_driven`.
        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        let effects = vec![e1, e2];

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [(view1.canonical_def.as_ref(), view1), (view2.canonical_def.as_ref(), view2)];
        freeze_install::seed_segment_cache_for_test(&cards, &primitives)
            .expect("two pointwise ColorGrades fuse across the seam");

        let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused-segment chain builds");
        assert!(!fused.pending_segments);
        let fused_kernels = fused
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        assert_eq!(
            fused_kernels, 1,
            "both cards must collapse into ONE cross-card kernel — every one \
             of card 2's inner nodes, including `clamp`, is fused away and \
             only reachable through the segment's retarget map"
        );

        run_once(&mut fused, &device, &input, &effects, &pc);
        let before = snapshot_output(&fused, &device, w, h);

        // Card 2's own (unprefixed) per-instance graph, with `clamp.max`
        // edited to clip the output hard. `clamp` carries no card-slider
        // binding (unlike gain/saturation/contrast/…, which ColorGrade DOES
        // bind — an edit there would just be re-asserted-over by the live
        // binding on the very next apply, proving nothing about the override
        // path itself). Bump `graph_version` only, NOT
        // `graph_structure_version`, so the runtime takes the in-place
        // override path instead of rebuilding.
        let mut effects2 = effects.clone();
        let mut edited = (*view2.canonical_def).clone();
        {
            use manifold_core::effect_graph_def::SerializedParamValue;
            let clamp = edited
                .nodes
                .iter_mut()
                .find(|n| n.node_id.as_str() == "clamp")
                .expect("ColorGrade has a `clamp` node");
            clamp
                .params
                .insert("max".to_string(), SerializedParamValue::Float { value: 0.05 });
        }
        effects2[1].graph = Some(edited);
        effects2[1].bump_graph_version();
        assert_eq!(
            effects2[1].graph_structure_version, effects[1].graph_structure_version,
            "sanity: this must be a value-only edit, not a rebuild"
        );

        run_once(&mut fused, &device, &input, &effects2, &pc);
        let differ = TextureDiff::new(&device);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "an inner-param edit on a fused SEGMENT member must reach the \
             live kernel (BUG-111) — clamping card 2's output to 0.05 must \
             visibly darken the frame: max_abs={}, over={}/{}",
            moved.max_abs,
            moved.over_count,
            moved.total
        );
    }

    /// D8/P7: a relight-on card must render identically whether the freeze
    /// compiler collapses it to one fused kernel or it runs per-atom. The
    /// fused path augments with DEFAULT knob values and writes live values
    /// per-frame; the unfused path splices the template with live values at
    /// build time. Both must land on the same pixels.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn relight_on_fused_matches_unfused_on_probe_graphs() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // both paths render inert, so this would pass vacuously — skip instead.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        for probe in [PresetTypeId::MIRROR, PresetTypeId::COLOR_GRADE] {
            let probe = probe.clone();
            let mut fx = make_default(probe.clone());
            set_param(&mut fx, "amount", 1.0);
            fx.relight = true;

            let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
            .expect("fused relight-on chain builds");
            assert!(!fused.pending_segments);
            let fused_kernel_count = fused
                .graph
                .nodes()
                .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
                .count();
            assert!(
                fused_kernel_count >= 1,
                "{probe:?}: relight-on card must use at least one fused kernel"
            );

            // Force the unfused path by watching the card: `should_render_fused`
            // returns false, so the relight template splices per-atom.
            let mut unfused = PresetRuntime::try_build(ChainBuildInputs { effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: Some(&fx.id) }, None)
            .expect("unfused relight-on chain builds");
            let unfused_kernel_count = unfused
                .graph
                .nodes()
                .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
                .count();
            assert_eq!(
                unfused_kernel_count, 0,
                "{probe:?}: watched card must not be fused"
            );

            run_once(&mut fused, &device, &input, std::slice::from_ref(&fx), &pc);
            run_once(&mut unfused, &device, &input, std::slice::from_ref(&fx), &pc);
            run_once(&mut fused, &device, &input, std::slice::from_ref(&fx), &pc);
            run_once(&mut unfused, &device, &input, std::slice::from_ref(&fx), &pc);

            let differ = TextureDiff::new(&device);
            let r = differ.compare(
                &device,
                fused.output_texture().unwrap(),
                unfused.output_texture().unwrap(),
                1.0e-2,
                3.0e-2,
            );
            assert!(
                r.passes(0.005) && r.over_count < 64,
                "{probe:?}: fused relight must match unfused relight: max_abs={}, over={}/{}",
                r.max_abs, r.over_count, r.total
            );
        }
    }

    /// D8/P7: float-knob edits are live uniforms, so dragging a knob on a
    /// fused relight-on card must visibly change the output without rebuilding
    /// the chain. This proves the per-frame `EffectSlot::relight_writes` path
    /// reaches the fused kernel.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn relight_knob_drag_visibly_changes_fused_output() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // no relight template is fused, so knob drags have no output to change.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut fx = make_default(PresetTypeId::MIRROR);
        set_param(&mut fx, "amount", 1.0);
        fx.relight = true;
        fx.relight_params.light_x = 0.7;
        fx.relight_params.light_y = -0.4;
        fx.relight_params.relief = 0.6;
        fx.relight_params.gain = 1.8;

        let mut cg = PresetRuntime::try_build(ChainBuildInputs { effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused relight-on chain builds");
        run_once(&mut cg, &device, &input, std::slice::from_ref(&fx), &pc);
        let before = snapshot_output(&cg, &device, w, h);

        fx.relight_params.light_x = -0.7;
        fx.relight_params.gain = 0.5;
        run_once(&mut cg, &device, &input, std::slice::from_ref(&fx), &pc);

        let differ = TextureDiff::new(&device);
        let moved = differ.compare(
            &device,
            &before.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "dragging relight knobs on a fused card must change the output: max_abs={}, over={}/{}",
            moved.max_abs, moved.over_count, moved.total
        );
    }

    /// D8/P7: the fused-view cache key must be knob-invariant for float D3
    /// knobs. Building a relight-on card with two different float-knob sets
    /// must hit the same cache entry; only `height_from` (topology) may mint
    /// a new entry.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn relight_float_knob_drag_hits_fused_view_cache() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // no relight template is fused, so there is no knob-invariant cache path.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let mut fx = make_default(PresetTypeId::COLOR_GRADE);
        fx.relight = true;

        // Prime the cache with default knobs.
        let _ = PresetRuntime::try_build(ChainBuildInputs { effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None)
        .expect("prime build");
        let cache_len_after_default =
            crate::node_graph::freeze::install::fused_effect_cache_len_for_test();

        // Move every float knob; the cache should NOT grow.
        fx.relight_params.light_x += 0.5;
        fx.relight_params.light_y -= 0.3;
        fx.relight_params.relief += 0.4;
        fx.relight_params.ao_intensity += 0.5;
        fx.relight_params.shadow_softness += 0.2;
        fx.relight_params.gain += 0.5;
        let _ = PresetRuntime::try_build(ChainBuildInputs { effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None)
        .expect("knob-drag build");
        let cache_len_after_knobs =
            crate::node_graph::freeze::install::fused_effect_cache_len_for_test();
        assert_eq!(
            cache_len_after_default, cache_len_after_knobs,
            "float-knob drag must be a fused-view cache HIT, not a new compile"
        );

        // `height_from` changes template topology: this MAY mint a new entry.
        fx.relight_params.height_from =
            manifold_core::effects::RelightHeightFrom::InvertedLuminance;
        let _ = PresetRuntime::try_build(ChainBuildInputs { effects: std::slice::from_ref(&fx), groups: &[], primitives: &primitives, device: &device, pool: None, width: 256, height: 256, preview_effect: None }, None)
        .expect("height-from build");
        let cache_len_after_height_from =
            crate::node_graph::freeze::install::fused_effect_cache_len_for_test();
        assert!(
            cache_len_after_height_from >= cache_len_after_knobs,
            "height_from is allowed to add a fused-view variant"
        );
    }

    /// D8/P7: a fused segment may now mix relight-on and relight-off members.
    /// The relight-on member is augmented with default params before the
    /// segment is concatenated, so the whole run fuses into one segment view.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn mixed_relight_segment_fuses_to_one_region() {
        // Relight disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // no relight template is spliced, so there is no mixed-region case to fuse.
        if !manifold_foundation::RELIGHT_FEATURE_ENABLED {
            return;
        }
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);

        let mut e1 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e1, "amount", 1.0);
        e1.relight = true;

        let mut e2 = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut e2, "amount", 1.0);
        e2.relight = false;

        let effects = vec![e1, e2];

        // Seed the segment cache so the chain builds fused (tests don't enqueue
        // the background worker).
        let cards = build_segment_cards(&[0, 1], &[(0, &effects[0]), (1, &effects[1])], &primitives);
        let card_refs: Vec<(&EffectGraphDef, &'static LoadedPresetView)> =
            cards.iter().map(|(d, v)| (d, *v)).collect();
        freeze_install::seed_segment_cache_for_test(&card_refs, &primitives)
            .expect("mixed ColorGrade segment fuses");

        let cg = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("mixed relight segment chain builds");
        assert!(!cg.pending_segments, "mixed segment must be ready after seeding");
        assert_eq!(
            cg.effect_nodes.len(),
            2,
            "one EffectSlot per member survives"
        );
        let fused_kernels = cg
            .graph
            .nodes()
            .filter(|n| n.node.type_id().as_str() == "node.wgsl_compute")
            .count();
        // The relight template cannot collapse to a single kernel — its blur
        // pair and GTAO are gather/camera cut points — so the strict claim is:
        // the segment path ran (one segment view, per-card path not taken),
        // the template's nodes are present, and BOTH template stretches fused
        // (the base+height region and the shading region).
        assert!(
            fused_kernels >= 2,
            "mixed relight-on/off segment must fuse both template regions, got {fused_kernels}"
        );
        assert!(
            cg.graph.nodes().any(|n| {
                crate::node_graph::relight::is_relight_node_id(n.node_id.as_str())
            }),
            "relight template nodes must be present in the fused segment graph"
        );
    }

    /// State harvest (docs/CHAIN_FUSION_DESIGN.md §5): rebuilding a chain
    /// with the prior runtime as donor must carry a feedback trail across the
    /// rebuild — the rebuilt chain continues exactly like a chain that never
    /// rebuilt. A rebuild WITHOUT the donor must visibly reset (sensitivity
    /// check: the trail actually accumulated something worth preserving).
    #[test]
    fn rebuild_with_prior_carries_feedback_trail_across() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        // StylizedFeedback (node.feedback trail in the StateStore) followed
        // by a ColorGrade — a realistic dial-in chain. Drive `rotate` so the
        // feedback trail genuinely evolves frame-to-frame: at the default
        // (zoom 0.95, rotate 0) a static self-similar gradient hits a
        // fixed point in one frame, so the output would be frame-invariant
        // and neither the harvest nor the sensitivity check would prove
        // anything. Rotation makes the prev spiral, so frame 1 ≠ frame 9.
        let mut fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        set_param(&mut fb, "amount", 1.0);
        set_param(&mut fb, "rotate", 10.0);
        set_param(&mut fb, "zoom", 0.9);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let effects = vec![fb, cg];

        let build = |prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, prior)
            .expect("chain builds")
        };

        const WARM: usize = 6;
        // Three post-rebuild frames, not one: a frozen ping-pong (the
        // shadowed-slot swap-failure class) still shows the carried trail on
        // frame 1 and only diverges once the state should have ADVANCED.
        const TAIL: usize = 3;
        // Reference: never rebuilt, runs WARM+TAIL frames.
        let mut reference = build(None);
        for _ in 0..WARM + TAIL {
            run_once(&mut reference, &device, &input, &effects, &pc);
        }

        // Harvested: WARM frames, rebuild WITH the prior, TAIL more frames.
        let mut donor = build(None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &effects, &pc);
        }
        let mut harvested = build(Some(&mut donor));
        for _ in 0..TAIL {
            run_once(&mut harvested, &device, &input, &effects, &pc);
        }

        // Reset: WARM frames, rebuild WITHOUT the prior, one more frame.
        let mut fresh_donor = build(None);
        for _ in 0..WARM {
            run_once(&mut fresh_donor, &device, &input, &effects, &pc);
        }
        let mut reset = build(None);
        run_once(&mut reset, &device, &input, &effects, &pc);

        let differ = TextureDiff::new(&device);
        let carried = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            harvested.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            carried.over_count, 0,
            "harvested rebuild must continue the trail exactly like an \
             un-rebuilt chain: max_abs={}, over={}/{}",
            carried.max_abs, carried.over_count, carried.total
        );
        let wiped = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            reset.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            wiped.over_count > 0,
            "sensitivity: a donor-less rebuild must visibly reset the trail \
             (otherwise this test proves nothing)"
        );
    }

    /// Repro harness for the 2026-06-11 on-stage report: Infrared →
    /// QuadMirror fused as a segment washed the frame to the palette's dark
    /// end. Fused segment vs per-card build of the same chain, real GPU.
    #[test]
    fn infrared_quadmirror_segment_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        let seeded = freeze_install::seed_segment_cache_for_test(&cards, &primitives);
        if seeded.is_none() {
            // No seam-spanning region — nothing fused, nothing to prove.
            return;
        }
        let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused chain builds");

        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused Infrared→QuadMirror segment must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// Repro for the 2026-06-11 follow-up report: same Infrared → QuadMirror
    /// chain, but with a NON-DEFAULT palette (Arctic, selector 6 — the setting
    /// in the on-stage screenshots). The shipped guard only proves palette 0;
    /// this drives the build-time value and a live palette switch.
    #[test]
    fn infrared_quadmirror_segment_nondefault_palette() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        let seeded = freeze_install::seed_segment_cache_for_test(&cards, &primitives);
        if seeded.is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused chain builds");

        run_once(&mut per_card, &device, &input, &effects, &pc);
        run_once(&mut fused, &device, &input, &effects, &pc);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused Infrared(Arctic)→QuadMirror must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );

        // Live palette switch on the fused chain: 6 → 2 (Green NV) must both
        // visibly change the output and still match per-card.
        let mut effects2 = effects.clone();
        set_param(&mut effects2[0], "palette", 2.0);
        let before = snapshot_output(&fused, &device, w, h);
        run_once(&mut fused, &device, &input, &effects2, &pc);
        let moved = differ.compare(
            &device,
            &before.texture,
            fused.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert!(
            moved.over_count > 0,
            "a live palette switch must visibly drive the fused chain"
        );
        run_once(&mut per_card, &device, &input, &effects2, &pc);
        let r2 = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r2.passes(0.005) && r2.over_count < 64,
            "after a live palette switch the fused chain must match per-card: \
             max_abs={}, over={}/{}",
            r2.max_abs,
            r2.over_count,
            r2.total
        );
    }

    /// Wireframe-like input: transparent black background (alpha 0), thin
    /// opaque white lines — the content class from the 2026-06-11 screenshots
    /// (generator wireframes), where Infrared→QuadMirror killed the frame but
    /// QuadMirror→Infrared rendered. The gradient repro (alpha 1 everywhere)
    /// passes, so alpha across the fused seam is the variable under test.
    fn wireframe_input(
        device: &manifold_gpu::GpuDevice,
        w: u32,
        h: u32,
    ) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let on_line = x % 32 < 2 || y % 32 < 2;
                let v = if on_line { 1.0 } else { 0.0 };
                px[i] = f16::from_f32(v);
                px[i + 1] = f16::from_f32(v);
                px[i + 2] = f16::from_f32(v);
                px[i + 3] = f16::from_f32(v); // alpha 0 off-line, like a generator
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "chain-fusion-wireframe-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Same fused-vs-per-card proof on the wireframe-like (alpha-0 background)
    /// input, both chain orders.
    #[test]
    fn infrared_quadmirror_segment_alpha_zero_background() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        // Deliberately NOT 256x256: the gradient_ramp LUT strip is 256 wide,
        // and a 256 canvas can mask cross-resolution sampling bugs by making
        // strip texels and canvas texels coincide.
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        for order in ["ir_qm", "qm_ir"] {
            let mut ir = make_default(PresetTypeId::INFRARED);
            set_param(&mut ir, "amount", 1.0);
            set_param(&mut ir, "palette", 6.0); // Arctic
            let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
            set_param(&mut qm, "amount", 1.0);
            let effects = if order == "ir_qm" {
                vec![ir, qm]
            } else {
                vec![qm, ir]
            };

            let mut per_card = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
            .expect("per-card chain builds");

            let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
            let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
            let cards = [
                (view1.canonical_def.as_ref(), view1),
                (view2.canonical_def.as_ref(), view2),
            ];
            if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
                continue;
            }
            let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
            .expect("fused chain builds");

            // Several STABLE frames: static-param specialization compiles a
            // baked variant once the value-key holds a frame and dispatches it
            // from then on — the steady-state path a live show actually runs.
            // One frame would only ever prove the generic kernel.
            for _ in 0..4 {
                run_once(&mut per_card, &device, &input, &effects, &pc);
                run_once(&mut fused, &device, &input, &effects, &pc);
            }
            let differ = TextureDiff::new(&device);
            let r = differ.compare(
                &device,
                per_card.output_texture().unwrap(),
                fused.output_texture().unwrap(),
                1.0e-2,
                3.0e-2,
            );
            assert!(
                r.passes(0.005) && r.over_count < 64,
                "[{order}] fused must match per-card on alpha-0 background: \
                 max_abs={}, over={}/{}",
                r.max_abs,
                r.over_count,
                r.total
            );
        }
    }

    /// The PRODUCTION swap sequence, end-to-end: build per-card (cold segment
    /// cache), render frames, the background compile lands, rebuild WITH the
    /// running chain as harvest donor, fused segment swaps in, keep rendering.
    /// The shipped guards seed the cache BEFORE the first build, so the
    /// mid-show swap-in (the path the app actually takes) was never proven.
    #[test]
    fn infrared_quadmirror_mid_show_swap_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let build = |prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, prior)
            .expect("chain builds")
        };

        // Per-card reference, never swapped.
        let mut reference = build(None);
        for _ in 0..6 {
            run_once(&mut reference, &device, &input, &effects, &pc);
        }

        // Production path: per-card frames, then the segment compile lands
        // and the chain rebuilds with the outgoing runtime as donor.
        let mut donor = build(None);
        for _ in 0..3 {
            run_once(&mut donor, &device, &input, &effects, &pc);
        }
        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut swapped = build(Some(&mut donor));
        for _ in 0..3 {
            run_once(&mut swapped, &device, &input, &effects, &pc);
        }

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            reference.output_texture().unwrap(),
            swapped.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "mid-show fused swap must match the per-card chain: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// The GraphTestsV4 layer-1 shape: a DISABLED card sits between the two
    /// enabled cards that fuse (Infrared ON → EdgeStretch OFF → QuadMirror ON).
    /// Segment fusion concatenates enabled cards across the gap; anything that
    /// indexes params by raw chain position would hand the fused kernel the
    /// disabled card's uniforms.
    #[test]
    fn fused_segment_spans_disabled_card_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut es = make_default(PresetTypeId::EDGE_STRETCH);
        es.enabled = false;
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, es, qm];

        let mut per_card = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[2].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused chain builds");

        for _ in 0..4 {
            run_once(&mut per_card, &device, &input, &effects, &pc);
            run_once(&mut fused, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment spanning a disabled card must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// Chain input at a DIFFERENT resolution than the canvas — the app feeds
    /// the chain a generator render target, which the resolution workstream
    /// can size below canvas. The fused kernel reads the chain source as an
    /// external (the cross-resolution sampling path); per-card resamples it
    /// node by node. An unfused QuadMirror in front normalizes resolution and
    /// would mask exactly this class, matching the order dependence reported.
    #[test]
    fn fused_segment_with_half_res_chain_input_matches_per_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w / 2, h / 2);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let mut qm = make_default(PresetTypeId::QUAD_MIRROR);
        set_param(&mut qm, "amount", 1.0);
        let effects = vec![ir, qm];

        let mut per_card = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("per-card chain builds");

        let view1 = loaded_preset_view_by_id(effects[0].effect_type()).unwrap();
        let view2 = loaded_preset_view_by_id(effects[1].effect_type()).unwrap();
        let cards = [
            (view1.canonical_def.as_ref(), view1),
            (view2.canonical_def.as_ref(), view2),
        ];
        if freeze_install::seed_segment_cache_for_test(&cards, &primitives).is_none() {
            return;
        }
        let mut fused = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("fused chain builds");

        for _ in 0..4 {
            run_once(&mut per_card, &device, &input, &effects, &pc);
            run_once(&mut fused, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            per_card.output_texture().unwrap(),
            fused.output_texture().unwrap(),
            1.0e-2,
            3.0e-2,
        );
        assert!(
            r.passes(0.005) && r.over_count < 64,
            "fused segment with half-res chain input must match per-card: \
             max_abs={}, over={}/{}",
            r.max_abs,
            r.over_count,
            r.total
        );
    }

    /// "Flash for a few frames then black" repro (2026-06-12, fusion OFF):
    /// Infrared ALONE on a STATIC input must produce a byte-identical frame
    /// every frame — it has no time dependence. The memo/hoisting path
    /// (gradient_ramp/mux/lut1d are pure+sticky) serves held LUT slots after
    /// the first frame; if a held slot is recycled/evicted/cleared the late
    /// frames go black while frame 0 was correct. Snapshot frame 0, run many
    /// frames, require the late frame to still match.
    #[test]
    fn infrared_alone_static_input_stays_stable_across_frames() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let effects = vec![ir];

        let mut cg = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("chain builds");

        // Frame 0 — the "flash" that looks correct.
        run_once(&mut cg, &device, &input, &effects, &pc);
        let frame0 = snapshot_output(&cg, &device, w, h);

        // Many more frames — the memo/sticky path is now serving held slots.
        for _ in 0..15 {
            run_once(&mut cg, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let drift = differ.compare(
            &device,
            &frame0.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            drift.over_count, 0,
            "Infrared on a static input must be frame-stable; a late frame \
             diverging from frame 0 is the flash-then-black bug: \
             max_abs={}, over={}/{}",
            drift.max_abs, drift.over_count, drift.total
        );
    }

    /// The on-stage blackout (2026-06-12): Infrared FOLLOWED BY another card,
    /// fusion off, static input. The chain plan's slot planner returns the
    /// sticky LUT resources' slots to its free pool at `free_after` (it only
    /// exempts persistent resources), so QuadMirror's intermediates share the
    /// LUT's physical texture and stomp it every frame — while the executor's
    /// memo skip keeps serving the latched slot. Infrared LAST works by
    /// accident (nothing runs after it to reuse the slot); this ordering is
    /// the one that goes black.
    #[test]
    fn infrared_before_quadmirror_stays_stable_across_frames() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (640u32, 360u32);
        let input = wireframe_input(&device, w, h);
        let pc = ctx(w, h);

        let mut ir = make_default(PresetTypeId::INFRARED);
        set_param(&mut ir, "amount", 1.0);
        set_param(&mut ir, "palette", 6.0); // Arctic
        let qm = make_default(PresetTypeId::QUAD_MIRROR);
        let effects = vec![ir, qm];

        let mut cg = PresetRuntime::try_build(ChainBuildInputs { effects: &effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, None)
        .expect("chain builds");

        run_once(&mut cg, &device, &input, &effects, &pc);
        let frame0 = snapshot_output(&cg, &device, w, h);

        for _ in 0..15 {
            run_once(&mut cg, &device, &input, &effects, &pc);
        }
        let differ = TextureDiff::new(&device);
        let drift = differ.compare(
            &device,
            &frame0.texture,
            cg.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            drift.over_count, 0,
            "Infrared before QuadMirror on a static input must be frame-stable; \
             late-frame divergence is the on-stage flash-then-black bug: \
             max_abs={}, over={}/{}",
            drift.max_abs, drift.over_count, drift.total
        );
    }

    /// Membership gate: a rebuild whose ACTIVE CARD SET changed (a card
    /// toggled off) must NOT harvest — the trail holds the removed card's
    /// look, and latching blends would freeze it on screen with no escape
    /// (the on-stage artifact class from 2026-06-11). Same-set rebuilds keep
    /// carrying.
    #[test]
    fn toggle_rebuild_resets_state_same_set_rebuild_carries() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let both = vec![fb.clone(), cg.clone()];
        // The toggled chain: ColorGrade disabled → not an active card.
        let mut cg_off = cg.clone();
        cg_off.enabled = false;
        let toggled = vec![fb.clone(), cg_off];

        let build = |effects: &[PresetInstance], prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(ChainBuildInputs { effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, prior)
            .expect("chain builds")
        };

        const WARM: usize = 6;
        // Donor accumulates a trail through BOTH cards, then the chain
        // rebuilds with ColorGrade toggled off.
        let mut donor = build(&both, None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &both, &pc);
        }
        let mut after_toggle = build(&toggled, Some(&mut donor));
        run_once(&mut after_toggle, &device, &input, &toggled, &pc);

        // Oracle: the toggled chain built fresh (what a reset looks like).
        let mut fresh = build(&toggled, None);
        run_once(&mut fresh, &device, &input, &toggled, &pc);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            fresh.output_texture().unwrap(),
            after_toggle.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "a toggle rebuild must reset state (match a fresh build), not \
             carry the old trail: max_abs={}, over={}/{}",
            r.max_abs, r.over_count, r.total
        );
    }

    /// Upstream-prefix gate: moving a card UPSTREAM of a stateful card
    /// changes what feeds it — its carried trail would be a stale picture of
    /// the old chain (the 2026-06-11 reorder artifact). The rebuild must
    /// reset exactly that card: [FB, CG] reordered to [CG, FB] makes the
    /// harvested chain match a fresh [CG, FB] build.
    #[test]
    fn upstream_reorder_resets_stateful_card() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = gradient_input(&device, w, h);
        let pc = ctx(w, h);

        let fb = make_default(PresetTypeId::STYLIZED_FEEDBACK);
        let mut cg = make_default(PresetTypeId::COLOR_GRADE);
        set_param(&mut cg, "amount", 1.0);
        set_param(&mut cg, "gain", 1.1);
        let fb_first = vec![fb.clone(), cg.clone()];
        let cg_first = vec![cg.clone(), fb.clone()];

        let build = |effects: &[PresetInstance], prior: Option<&mut PresetRuntime>| {
            PresetRuntime::try_build(ChainBuildInputs { effects, groups: &[], primitives: &primitives, device: &device, pool: None, width: w, height: h, preview_effect: None }, prior)
            .expect("chain builds")
        };

        const WARM: usize = 6;
        let mut donor = build(&fb_first, None);
        for _ in 0..WARM {
            run_once(&mut donor, &device, &input, &fb_first, &pc);
        }
        let mut reordered = build(&cg_first, Some(&mut donor));
        run_once(&mut reordered, &device, &input, &cg_first, &pc);

        let mut fresh = build(&cg_first, None);
        run_once(&mut fresh, &device, &input, &cg_first, &pc);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            fresh.output_texture().unwrap(),
            reordered.output_texture().unwrap(),
            1.0e-3,
            1.0e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "an upstream reorder must reset the feedback card (match a fresh \
             build): max_abs={}, over={}/{}",
            r.max_abs, r.over_count, r.total
        );
    }
