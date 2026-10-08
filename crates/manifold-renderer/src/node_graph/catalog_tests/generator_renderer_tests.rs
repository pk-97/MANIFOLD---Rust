    use crate::generator_renderer::{GeneratorRenderer, testkit::GeneratorRendererTestkit};
    use crate::generator_renderer::state::ActiveClip;
    use manifold_core::{ClipId, PresetTypeId, LayerId};
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_gpu::GpuTextureFormat;

    /// Architectural regression: a generator type swap mid-clip must
    /// re-build the per-layer `Generator` against the host's *current*
    /// canvas dimensions AND mark every active clip on that layer for
    /// a render-target clear before the new generator's first frame.
    ///
    /// Pre-fix, `update_active_types_for_layer` and the per-frame
    /// override-version sweep called the registry directly with
    /// hardcoded 1920×1080 and never touched `ActiveClip::needs_clear`.
    /// Two visible failure modes:
    /// 1. At any host resolution other than 1920×1080, the new
    ///    generator's `canvas_sized_array_outputs` (scatter
    ///    accumulators, density grids) allocated at 1920×1080 and the
    ///    dispatch sized from `Backend::canvas_dims()` mapped splats
    ///    into a sub-rect of the real canvas — Strange Attractor
    ///    rendered into the top-left quadrant only.
    /// 2. The canvas-sized output texture still held the previous
    ///    generator's last frame; wherever the new generator didn't
    ///    write (sparse particle splats, narrow wireframes), the old
    ///    generator's pixels stayed visible — the user-reported
    ///    "leaves an artifact of the previous generator" bug.
    ///
    /// Both invariants now hold by construction because
    /// `install_layer_generator` is the only path that mutates
    /// `layer_generators`, and it (a) passes `self.width/height` into
    /// `GeneratorRegistry::create_with_override` (which takes canvas
    /// dims as required arguments — no silent default), and (b)
    /// dirties every `active_clips` entry for the affected layer.
    ///
    /// This test exercises the *swap* path (the one that was
    /// fundamentally broken) at a non-default host resolution.
    #[test]
    fn generator_type_swap_marks_active_clips_for_clear_at_host_canvas_dims() {
        let device = manifold_gpu::testkit::test_device();
        let host_w: u32 = 1280;
        let host_h: u32 = 720;

        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.arc(),
            host_w,
            host_h,
            GpuTextureFormat::Rgba16Float,
            0,
        );

        let layer_id = LayerId::new("layer-under-test");
        let other_layer = LayerId::new("other-layer");
        // TrivialPassthrough moved to test fixtures (PRESET_BROWSER_AUDITION
        // P1, D8) — these mechanics tests use Plasma, a bundled JSON preset.
        let plasma = PresetTypeId::new("Plasma");
        let strange = PresetTypeId::new("StrangeAttractor");

        // Seed a `LayerGeneratorState` for the starting type via the
        // same funnel any production path would use. (Any JSON preset
        // works; the test doesn't render it, it just exists so the
        // swap path has something to replace.)
        assert!(
            renderer.install_layer_generator(
                layer_id.clone(),
                plasma.clone(),
                None,
                None,
                None,
                0,
                0,
                std::collections::BTreeMap::new(),
                None,
                false,
                manifold_core::effects::RelightParams::default(),
            ),
            "seed install of Plasma must succeed",
        );

        // Two active clips on the layer at non-default canvas dims.
        // Manually populated so the test doesn't depend on the
        // `start_clip` plumbing — the invariant under test is that
        // `install_layer_generator` reaches every active clip on the
        // layer regardless of how it got there.
        for tag in ["clip-a", "clip-b"] {
            let rt = RenderTarget::new(
                &device,
                host_w,
                host_h,
                GpuTextureFormat::Rgba16Float,
                "test RT",
            );
            renderer.active_clips.insert(
                ClipId::new(tag),
                ActiveClip {
                    render_target: rt,
                    generator_type: plasma.clone(),
                    layer_id: layer_id.clone(),
                    layer_index: 0,
                    clip_index: 0,
                    anim_progress: 0.0,
                    // Pretend the first frame already cleared the
                    // flag. The swap must re-dirty both clips.
                    needs_clear: false,
                },
            );
        }
        // One clip on a *different* layer that must NOT be touched
        // by the swap. Catches the "iterate every active clip"
        // footgun (over-clearing other layers).
        let other_rt = RenderTarget::new(
            &device,
            host_w,
            host_h,
            GpuTextureFormat::Rgba16Float,
            "other RT",
        );
        renderer.active_clips.insert(
            ClipId::new("clip-other"),
            ActiveClip {
                render_target: other_rt,
                generator_type: plasma.clone(),
                layer_id: other_layer.clone(),
                layer_index: 1,
                clip_index: 0,
                anim_progress: 0.0,
                needs_clear: false,
            },
        );

        // === The swap ===
        renderer.update_active_types_for_layer(&layer_id, strange.clone());

        // Invariant 1: the rebuild used host canvas dims, not a
        // hardcoded default. If a future regression silently drops
        // the dims again, this assertion fails before any visual bug
        // can ship.
        {
            let layer_state = renderer
                .layer_generators
                .get_mut(&layer_id)
                .expect("layer state must exist after swap");
            assert_eq!(
                layer_state.generator_type, strange,
                "swap must install the new generator type",
            );
            let json_gen = layer_state.generator.as_ref();
            assert_eq!(
                json_gen.backend_for_test().canvas_dims(),
                (host_w, host_h),
                "post-swap generator's backend must report host canvas dims, \
                 not the registry's pre-fix hardcoded default",
            );
        }

        // Invariant 2: every active clip on the swapped layer is
        // dirty; the unrelated layer's clip is untouched.
        let clip_a = renderer
            .active_clips
            .get("clip-a")
            .expect("clip-a must remain active after swap");
        assert!(
            clip_a.needs_clear,
            "clip-a on the swapped layer must be marked for clear",
        );
        let clip_b = renderer
            .active_clips
            .get("clip-b")
            .expect("clip-b must remain active after swap");
        assert!(
            clip_b.needs_clear,
            "clip-b on the swapped layer must be marked for clear",
        );
        let clip_other = renderer
            .active_clips
            .get("clip-other")
            .expect("clip-other must remain active");
        assert!(
            !clip_other.needs_clear,
            "swap on layer-under-test must not touch clips on other layers",
        );
    }

    /// section 8 D1 — the generator half of the P2 gate (the effect-chain half is
    /// `preset_runtime::generator_input_tests::run_feeds_nonzero_trigger_count_into_generator_input_effect_slot`).
    /// `effective_trigger_count` sums `clip_count` (clip-launch edge) +
    /// `audio_count` (audio-trigger fires), and the clip edge is mode-gated
    /// at `acquire_clip` time by `clip_edge_enabled` — `Transient`-only mode
    /// (simulated here by passing `false`) must NOT bump `clip_count`.
    #[test]
    fn effective_trigger_count_sums_clip_and_audio_and_respects_clip_edge_mode() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.arc(), 256, 256, GpuTextureFormat::Rgba16Float, 0,
        );
        let layer_id = LayerId::new("trigger-count-layer");
        // TrivialPassthrough moved to test fixtures (PRESET_BROWSER_AUDITION
        // P1, D8) — Plasma is the bundled stand-in for mechanics tests.
        let gen_type = PresetTypeId::new("Plasma");

        assert!(
            renderer.install_layer_generator(
                layer_id.clone(),
                gen_type.clone(),
                None,
                None,
                None,
                0,
                0,
                std::collections::BTreeMap::new(),
                None,
                false,
                manifold_core::effects::RelightParams::default(),
            ),
            "seed install must succeed",
        );

        // Two clip launches (clip_edge_enabled = true, the default/no-config
        // behavior): clip_count 0 -> 2.
        assert!(renderer.acquire_clip(
            "clip-1",
            gen_type.clone(),
            layer_id.clone(),
            0,
            0,
            None,
            0,
            0,
            true,
            None,
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        assert!(renderer.acquire_clip(
            "clip-2",
            gen_type.clone(),
            layer_id.clone(),
            0,
            1,
            None,
            0,
            0,
            true,
            None,
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        assert_eq!(
            renderer.layer_generators.get(&layer_id).unwrap().clip_count,
            2,
            "two distinct clip launches with clip edge enabled must bump clip_count twice",
        );

        // Three audio-trigger fires: audio_count 0 -> 3.
        renderer.bump_audio_count(&layer_id);
        renderer.bump_audio_count(&layer_id);
        renderer.bump_audio_count(&layer_id);
        assert_eq!(
            renderer.effective_trigger_count_for_layer(&layer_id),
            5,
            "effective count must be clip_count(2) + audio_count(3) = 5",
        );

        // A third clip launch with clip_edge_enabled = false (Transient-only
        // mode) must NOT bump clip_count — the whole point of D1's mode gate.
        assert!(renderer.acquire_clip(
            "clip-3",
            gen_type,
            layer_id.clone(),
            0,
            2,
            None,
            0,
            0,
            false,
            None,
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        assert_eq!(
            renderer.layer_generators.get(&layer_id).unwrap().clip_count,
            2,
            "Transient-only mode must silently ignore the clip-launch edge",
        );
        assert_eq!(
            renderer.effective_trigger_count_for_layer(&layer_id),
            5,
            "effective count unchanged by the mode-gated-off clip launch",
        );

        // A layer with no generator reads 0, not a panic.
        assert_eq!(
            renderer.effective_trigger_count_for_layer(&LayerId::new("no-such-layer")),
            0,
        );
    }
