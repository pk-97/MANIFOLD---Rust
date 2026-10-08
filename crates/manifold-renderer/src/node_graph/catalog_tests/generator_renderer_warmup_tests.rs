    use crate::generator_renderer::{GeneratorRenderer, testkit::GeneratorRendererTestkit};
    use crate::generator_renderer::state::ThumbGen;
    use crate::generator_renderer::testkit::{THUMB_W, THUMB_H};
    use manifold_node_engine::runtime::PresetRuntime;
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_core::{ClipId, LayerId};
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;
    use manifold_core::{Beats, LayerType, PresetTypeId, Seconds};
    use manifold_foundation::cold_touch::{
        reset_cold_touch_counts, set_transport_playing, total_cold_touches,
    };
    use manifold_gpu::GpuTextureFormat;
    use manifold_playback::renderer::ClipRenderer;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const CANVAS_W: u32 = 640;
    const CANVAS_H: u32 = 360;

    fn apricot_fixture_path() -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../../tests/fixtures/rt/apricot_tl05.glb");
        path
    }

    fn apricot_weather_layer(model_path: &str) -> Layer {
        let mut layer = Layer::new("Apricot".to_string(), LayerType::Generator, 0);
        layer.change_generator_type(PresetTypeId::new("BlossomWire"));
        let mut clip = TimelineClip::new_generator(Beats(0.0), Beats(8.0));
        let mut strings = BTreeMap::new();
        strings.insert("modelPath".to_string(), model_path.to_string());
        clip.string_params = Some(strings);
        layer.clips.push(clip);
        layer
    }

    fn project_with_apricot_scene() -> Project {
        let mut project = Project::default();
        project.settings.output_width = CANVAS_W as i32;
        project.settings.output_height = CANVAS_H as i32;
        project
            .timeline
            .layers
            .push(apricot_weather_layer(&apricot_fixture_path().to_string_lossy()));
        project
    }

    fn insert_test_thumb(
        renderer: &mut GeneratorRenderer,
        clip_id: &str,
        ready: bool,
        status: manifold_node_engine::runtime::frame_status::FrameRenderStatus,
    ) {
        let gen_type = PresetTypeId::new("Plasma");
        let runtime = (renderer.registry.create)(
                renderer.device.clone(),
                renderer.format,
                &gen_type,
                None,
                THUMB_W,
                THUMB_H,
                false,
                None,
                None,
            )
            .expect("Plasma thumbnail runtime");
        let rt = RenderTarget::new(
            &renderer.device,
            THUMB_W,
            THUMB_H,
            renderer.format,
            "thumbnail ownership test",
        );
        renderer.thumb_gens.insert(
            ClipId::new(clip_id),
            ThumbGen {
                runtime,
                rt,
                gen_type,
                ready,
                frame_count: 45,
                last_frame_status: status,
            },
        );
    }

    /// A node error leaves a fallback frame on display; only a simulation or
    /// geometry failure hides it.
    #[test]
    fn node_error_frame_stays_presentable() {
        use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        insert_test_thumb(&mut renderer, "errored", true, FrameRenderStatus::Failed(FrameRenderFailure::NodeError));
        insert_test_thumb(&mut renderer, "broken", true, FrameRenderStatus::Failed(FrameRenderFailure::Simulation));
        assert!(renderer.thumb_texture("errored").is_some());
        assert!(renderer.thumb_texture("broken").is_none());
    }

    #[test]
    fn thumbnail_pruning_handles_captured_offscreen_and_equal_size_changes() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        insert_test_thumb(
            &mut renderer,
            "captured",
            true,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
        );
        insert_test_thumb(
            &mut renderer,
            "offscreen",
            true,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
        );
        insert_test_thumb(
            &mut renderer,
            "replacement",
            false,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::PendingGeometry,
        );

        let visible = [ClipId::new("captured"), ClipId::new("replacement")];
        let captured = [ClipId::new("captured")];
        renderer.evict_thumb_gens(|id| visible.contains(id) && !captured.contains(id));

        assert!(!renderer.thumb_gens.contains_key("captured"));
        assert!(!renderer.thumb_gens.contains_key("offscreen"));
        assert!(renderer.thumb_gens.contains_key("replacement"));
        assert_eq!(renderer.thumb_gens.len(), 1);

        // Equal cardinality must still prune a changed set: only `same-live`
        // is retained even though the candidate list has three entries and
        // names a different set from the parked map.
        insert_test_thumb(
            &mut renderer,
            "same-live",
            true,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
        );
        insert_test_thumb(
            &mut renderer,
            "same-stale-a",
            true,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
        );
        let equal_size_visible = [
            ClipId::new("same-live"),
            ClipId::new("new-visible-a"),
            ClipId::new("new-visible-b"),
        ];
        renderer.evict_thumb_gens(|id| equal_size_visible.contains(id));
        assert!(renderer.thumb_gens.contains_key("same-live"));
        assert!(!renderer.thumb_gens.contains_key("same-stale-a"));
    }

    #[test]
    fn pending_visible_thumbnail_is_retained_and_live_generator_is_independent() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        let layer_id = LayerId::new("live-layer");
        assert!(renderer.install_layer_generator(
            layer_id.clone(),
            PresetTypeId::new("Plasma"),
            None,
            None,
            None,
            0,
            0,
            BTreeMap::new(),
            None,
            false,
            manifold_core::effects::RelightParams::default(),
        ));
        insert_test_thumb(
            &mut renderer,
            "pending-visible",
            false,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::PendingGeometry,
        );

        let visible = [ClipId::new("pending-visible")];
        renderer.evict_thumb_gens(|id| visible.contains(id));
        assert!(renderer.thumb_gens.contains_key("pending-visible"));
        let live = &*renderer.layer_generators[&layer_id].generator as *const PresetRuntime;
        renderer.evict_thumb_gens(|_| false);
        assert!(renderer.thumb_gens.is_empty());
        assert!(renderer.layer_generators.contains_key(&layer_id));
        assert_eq!(&*renderer.layer_generators[&layer_id].generator as *const PresetRuntime, live);
    }

    #[test]
    fn thumbnail_capture_survives_owner_drop_before_gpu_submission() {
        let _serial = manifold_gpu::testkit::test_device();
        // Independent retirement owner: do not change the shared test device.
        let device = manifold_node_engine::gpu::context::test_gpu_device("generator_renderer tests");
        let event = device.create_event();
        let (sender, mut retirement) = manifold_gpu::RetireQueue::new();
        device.set_retirement(manifold_gpu::RetireMark::new(event.second_handle(), sender));
        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.clone(), 64, 64, GpuTextureFormat::Rgba8Unorm, 0,
        );
        insert_test_thumb(
            &mut renderer, "captured", true,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete,
        );
        let atlas = RenderTarget::new(
            &device, THUMB_W, THUMB_H, GpuTextureFormat::Rgba8Unorm,
            "thumbnail capture destination",
        );
        let bytes_per_row = THUMB_W * 4;
        let readback = device.create_buffer_shared(u64::from(bytes_per_row * THUMB_H));
        let mut encoder = device.create_encoder("thumbnail capture retirement");
        let source = renderer.thumb_texture("captured").unwrap();
        encoder.clear_texture(source, 1.0, 0.0, 0.0, 1.0);
        encoder.copy_texture_to_texture(source, &atlas.texture, THUMB_W, THUMB_H, 1);
        renderer.evict_thumb_gens(|_| false);
        retirement.drain();
        assert!(retirement.pending_count() > 0, "dropped owners await the frame fence");
        assert!(renderer.thumb_gens.is_empty());
        encoder.copy_texture_to_buffer(
            &atlas.texture, &readback, THUMB_W, THUMB_H, bytes_per_row,
        );
        encoder.signal_event(&event);
        encoder.commit_and_wait_completed();
        retirement.drain();
        assert_eq!(retirement.pending_count(), 0);
        // SAFETY: the completed command buffer owns the only GPU writes;
        // readback is shared and the slice exactly matches its allocation.
        let pixels = unsafe {
            std::slice::from_raw_parts(
                readback.mapped_ptr().unwrap(), (bytes_per_row * THUMB_H) as usize,
            )
        };
        assert!(pixels.chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]));
    }

    #[test]
    fn thumbnail_output_is_withheld_until_ready_and_frame_complete() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new_unwarmed(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        insert_test_thumb(
            &mut renderer,
            "pending",
            false,
            manifold_node_engine::runtime::frame_status::FrameRenderStatus::PendingGeometry,
        );
        assert!(renderer.thumb_texture("pending").is_none());
        {
            let thumb = renderer.thumb_gens.get_mut("pending").unwrap();
            thumb.ready = true;
            thumb.last_frame_status = manifold_node_engine::runtime::frame_status::FrameRenderStatus::Complete;
        }
        assert!(renderer.thumb_texture("pending").is_some());
        renderer
            .thumb_gens
            .get_mut("pending")
            .unwrap()
            .last_frame_status = manifold_node_engine::runtime::frame_status::FrameRenderStatus::PendingGeometry;
        assert!(renderer.thumb_texture("pending").is_none());
    }

    fn render_frames(renderer: &mut GeneratorRenderer, layers: &[Layer], frames: usize) {
        let device = renderer.device.clone();
        const DT: f32 = 1.0 / 60.0;
        for f in 0..frames {
            let mut native_enc = device.create_encoder("warmup_test");
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);
            let time = f as f64 * DT as f64;
            renderer.render_all(&mut gpu, time, 0.0, DT, layers, 1, &[], None);
            native_enc.commit_and_wait_completed();
            renderer.uniform_arena.flush(&device);
        }
    }

    /// INV1 — after the warmup pass, playing the scene must not trigger any
    /// cold-touch counter site (pipeline compile, GLB parse, HDRI decode,
    /// model load, chain construction).
    #[test]
    fn warmup_gate_zero_cold_touches_during_playback() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        let project = project_with_apricot_scene();
        let layer = &project.timeline.layers[0];

        // Warmup is where first-touch costs are expected and logged.
        let outcome = renderer.prewarm_layer(layer, manifold_core::WarmupBudget::default());
        assert!(
            matches!(outcome, manifold_core::WarmupOutcome::Quiescent),
            "fixture scene must warm within default budget; got {:?}",
            outcome
        );

        // From here on we are "on stage": any cold touch is a policy breach.
        reset_cold_touch_counts();
        set_transport_playing(true);

        let clip = &layer.clips[0];
        assert!(
            renderer.start_clip(clip, Seconds(0.0), &project.timeline.layers, 0, true),
            "first launch of the warmed layer must acquire its existing generator"
        );
        render_frames(&mut renderer, &project.timeline.layers, 60);

        assert_eq!(
            total_cold_touches(),
            0,
            "zero cold touches during 60 frames of playback after warmup"
        );
        set_transport_playing(false);
    }

    /// INV2 — a layer whose async warmup work never finishes within the
    /// per-layer wall-clock budget must terminate with `BudgetExhausted` rather
    /// than blocking open indefinitely. We use the real BlossomWire scene
    /// with a 1ns wall-clock budget: the cap trips before the GLB parse can
    /// quiesce, even if a disk cache makes the parse fast on the second run.
    #[test]
    fn warmup_inv2_budget_terminates_never_quiescent() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        let project = project_with_apricot_scene();
        let layer = &project.timeline.layers[0];

        let tight_budget = manifold_core::WarmupBudget {
            per_layer: std::time::Duration::from_nanos(1),
            per_layer_frames: 600,
            total: std::time::Duration::from_secs(60),
        };
        let outcome = renderer.prewarm_layer(layer, tight_budget);
        assert!(
            matches!(
                outcome,
                manifold_core::WarmupOutcome::BudgetExhausted { .. }
            ),
            "1ns wall-clock budget must exhaust before the GLB parse quiesces"
        );
    }

    /// INV3 — after warmup, the first live clip launch must hit the existing
    /// per-layer generator entry instead of rebuilding it.
    #[test]
    fn warmup_inv3_acquire_clip_hits_installed_generator() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        let project = project_with_apricot_scene();
        let layer = &project.timeline.layers[0];
        let layer_id = layer.layer_id.clone();

        let outcome = renderer.prewarm_layer(layer, manifold_core::WarmupBudget::default());
        assert!(
            matches!(outcome, manifold_core::WarmupOutcome::Quiescent),
            "fixture scene must warm within default budget; got {:?}",
            outcome
        );
        assert!(
            renderer.layer_generators.contains_key(&layer_id),
            "warmup must leave a generator installed for the layer"
        );

        reset_cold_touch_counts();
        set_transport_playing(true);

        let clip = &layer.clips[0];
        assert!(
            renderer.start_clip(clip, Seconds(0.0), &project.timeline.layers, 0, true),
            "first launch must acquire the warmed generator"
        );
        render_frames(&mut renderer, &project.timeline.layers, 1);

        assert!(
            renderer.layer_generators.contains_key(&layer_id),
            "first launch must not tear down the warmed generator"
        );
        assert_eq!(
            renderer.active_count(),
            1,
            "first launch must create exactly one active clip"
        );
        assert_eq!(
            total_cold_touches(),
            0,
            "no construction work during the cache-hit launch"
        );
        set_transport_playing(false);
    }

    /// D7 edit-time add-warmup: assigning a generator to a layer while the
    /// transport is stopped must warm that layer through the same prewarm_layer
    /// seam, leaving `layer_generators` populated.
    #[test]
    fn edit_time_generator_assignment_warms_when_stopped() {
        let device = manifold_gpu::testkit::test_device();
        let mut renderer = GeneratorRenderer::new(
            device.arc(),
            CANVAS_W,
            CANVAS_H,
            GpuTextureFormat::Rgba16Float,
            0,
        );
        let mut layer = Layer::new("Edit".to_string(), LayerType::Generator, 0);
        // TrivialPassthrough moved to test fixtures (PRESET_BROWSER_AUDITION
        // P1, D8) — prewarm resolves through the live registry, so this uses
        // the bundled Plasma preset.
        layer.change_generator_type(PresetTypeId::new("Plasma"));
        let layer_id = layer.layer_id.clone();

        // Simulate the command path: type change notification, then warm.
        renderer.update_active_types_for_layer(&layer_id, layer.generator_type().clone());
        let outcome = renderer.prewarm_layer(&layer, manifold_core::WarmupBudget::default());
        assert!(
            matches!(outcome, manifold_core::WarmupOutcome::Quiescent),
            "edit-time warm of Plasma must quiesce; got {:?}",
            outcome
        );
        assert!(
            renderer.layer_generators.contains_key(&layer_id),
            "edit-time generator assignment must leave the layer warm"
        );
    }
