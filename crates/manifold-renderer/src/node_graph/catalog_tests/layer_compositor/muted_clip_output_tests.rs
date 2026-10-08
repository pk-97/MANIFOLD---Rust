
    //! Regression: a layer whose clips are ALL muted must emit no
    //! `LayerOutput`. Before the fix, the multi-clip / has-layer-effects
    //! branch pushed the empty (transparent) layer buffer unconditionally —
    //! under Opaque blend, which replaces every pixel regardless of alpha,
    //! one all-muted Opaque layer blacked out the entire frame (hot-mute
    //! regression: muted clips flow into clip descriptors, this branch
    //! never learned to handle the all-muted group).
    use crate::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
    use crate::compositor::CompositorFrame;
    use manifold_core::effects::PresetInstance;
    use manifold_core::{BlendMode, LayerId, PresetTypeId};
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_gpu::{GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
    use crate::compositor::CompositeLayerDescriptor;

    fn make_layer_desc<'a>(
        layer_id: &'a LayerId,
        effects: &'a [PresetInstance],
    ) -> CompositeLayerDescriptor<'a> {
        CompositeLayerDescriptor {
            layer_index: 0,
            layer_id,
            blend_mode: BlendMode::Opaque,
            opacity: 1.0,
            hidden: false,
            blit_to_led: false,
            layer_type: manifold_core::LayerType::Video,
            effects,
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
        }
    }

    fn make_clip<'a>(clip_id: &'a str, texture: &'a GpuTexture, muted: bool) -> CompositeClipDescriptor<'a> {
        CompositeClipDescriptor {
            clip_id,
            texture,
            layer_index: 0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: muted,
            effects: &[],
            effect_groups: &[],
        }
    }

    fn make_fx() -> PresetInstance {
        let mut fx = manifold_core::preset_definition_registry::create_default(
            &PresetTypeId::MIRROR,
        );
        if let Some(p) = fx.params.iter_mut().next() {
            p.value = 1.0;
        }
        fx
    }

    fn run(
        comp: &mut LayerCompositor,
        device: &manifold_gpu::testkit::TestDevice,
        layers: &[CompositeLayerDescriptor],
        clips: &[CompositeClipDescriptor],
    ) {
        let frame = CompositorFrame {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            project_tempo: None,
            frame_count: 1,
            compositor_dirty: true,
            clips,
            layers,
            master_effects: &[],
            master_effect_groups: &[],
            master_trigger_count: 0,
            tonemap: crate::tonemap::TonemapSettings::default(),
            led_exit_index: -1,
            led_composite_size: (8, 120),
            output_width: 64,
            output_height: 64,
            occluded_layers: &[],
            render_skip: &[],
        };
        let mut native_enc = device.create_encoder("muted-clip-output-test");
        {
            let mut gpu = GpuEncoder::new(&mut native_enc, device);
            comp.generate_layers(&mut gpu, &frame);
        }
        native_enc.commit_and_wait_completed();
    }

    fn white_texture(device: &manifold_gpu::testkit::TestDevice) -> GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: 64,
            height: 64,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "muted-clip-test-src",
            mip_levels: 1,
        })
    }

    #[test]
    fn all_muted_clips_with_layer_effects_emit_no_output() {
        let device = manifold_gpu::testkit::test_device();
        let mut comp = LayerCompositor::new(&device, 64, 64);
        let layer_id = LayerId::from("L0");
        let fx = [make_fx()];
        let layers = [make_layer_desc(&layer_id, &fx)];
        let tex = white_texture(&device);
        let clips = [make_clip("c0", &tex, true)];

        run(&mut comp, &device, &layers, &clips);
        assert!(
            comp.layer_outputs_scratch.is_empty(),
            "all-muted layer must emit no LayerOutput — an empty buffer under \
             Opaque blend blacks out the whole frame",
        );
    }

    #[test]
    fn visible_clip_with_layer_effects_still_emits() {
        let device = manifold_gpu::testkit::test_device();
        let mut comp = LayerCompositor::new(&device, 64, 64);
        let layer_id = LayerId::from("L0");
        let fx = [make_fx()];
        let layers = [make_layer_desc(&layer_id, &fx)];
        let tex = white_texture(&device);
        // Mixed group: one muted, one visible — the layer must emit.
        let clips = [make_clip("c0", &tex, true), make_clip("c1", &tex, false)];

        run(&mut comp, &device, &layers, &clips);
        assert_eq!(
            comp.layer_outputs_scratch.len(),
            1,
            "a layer with any visible clip keeps its output",
        );
    }
