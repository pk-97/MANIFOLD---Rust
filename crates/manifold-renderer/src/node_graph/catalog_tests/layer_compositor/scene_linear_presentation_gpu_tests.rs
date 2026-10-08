
    //! Production compositor proofs: master effects run on SceneLinear HDR,
    //! then the destination presentation pass applies the optional SDR curve.

    use crate::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
    use crate::compositor::CompositeLayerDescriptor;
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_node_engine::gpu::render_target::RenderTarget;
    use manifold_gpu::{GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
    use crate::compositor::{Compositor, CompositorFrame};
    use manifold_node_engine::gpu::headless_readback::readback_raw_halves;
    use crate::presentation::{
        DisplayCapabilities, DisplayPlan, LinearPresentationTarget, LinearSceneFrame,
        PresentationPipeline, UI_FORMAT,
    };
    use crate::tonemap::{TonemapMode, TonemapSettings};
    use half::f16;
    use manifold_core::{BlendMode, LayerId, PresetTypeId, TonemapCurve};
    use manifold_gpu::GpuLoadAction;

    const WIDTH: u32 = 4;
    const HEIGHT: u32 = 4;

    #[derive(Clone, Copy)]
    enum MasterEffect {
        None,
        Invert,
        ColorGradeGain,
    }

    struct RenderSample {
        hdr: [f32; 4],
        mapped: [f32; 4],
    }

    fn solid_source(device: &manifold_gpu::testkit::TestDevice, rgb: [f32; 3]) -> GpuTexture {
        let texture = device.create_texture(&GpuTextureDesc {
            width: WIDTH,
            height: HEIGHT,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: "scene-linear-presentation-source",
            mip_levels: 1,
        });
        let mut encoder = device.create_encoder("scene-linear-presentation-source-clear");
        {
            let mut gpu = GpuEncoder::new(&mut encoder, device);
            gpu.clear_texture(&texture, rgb[0] as f64, rgb[1] as f64, rgb[2] as f64, 1.0);
        }
        encoder.commit_and_wait_completed();
        texture
    }

    fn first_pixel(raw: &[u8]) -> [f32; 4] {
        std::array::from_fn(|channel| {
            let offset = channel * 2;
            f16::from_bits(u16::from_le_bytes([raw[offset], raw[offset + 1]])).to_f32()
        })
    }

    fn render_sample(
        source_rgb: [f32; 3],
        curve: TonemapCurve,
        master_effect_kind: MasterEffect,
    ) -> RenderSample {
        let device = manifold_gpu::testkit::test_device();
        let mut compositor = LayerCompositor::new(&device, WIDTH, HEIGHT);
        let source = solid_source(&device, source_rgb);
        let layer_id = LayerId::from("scene-linear-presentation-layer");
        let layer = CompositeLayerDescriptor {
            layer_index: 0,
            layer_id: &layer_id,
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
        };
        let clip = CompositeClipDescriptor {
            clip_id: "scene-linear-presentation-clip",
            texture: &source,
            layer_index: 0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: false,
            effects: &[],
            effect_groups: &[],
        };
        let mut master_effect = match master_effect_kind {
            MasterEffect::None => None,
            MasterEffect::Invert => Some(
                manifold_core::preset_definition_registry::create_default(
                    &PresetTypeId::INVERT_COLORS,
                ),
            ),
            MasterEffect::ColorGradeGain => Some(
                manifold_core::preset_definition_registry::create_default(
                    &PresetTypeId::COLOR_GRADE,
                ),
            ),
        };
        if let Some(effect) = master_effect.as_mut() {
            effect.enabled = true;
            match master_effect_kind {
                MasterEffect::None => unreachable!("None has no master effect"),
                MasterEffect::Invert => {
                    effect.params.get_mut("amount").expect("Invert amount").value = 1.0;
                }
                MasterEffect::ColorGradeGain => {
                    effect.params.get_mut("amount").expect("ColorGrade amount").value = 1.0;
                    effect.params.get_mut("gain").expect("ColorGrade gain").value = 2.0;
                }
            }
        }
        let master_effects = master_effect.as_slice();
        let frame = CompositorFrame {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            project_tempo: None,
            frame_count: 1,
            compositor_dirty: true,
            clips: std::slice::from_ref(&clip),
            layers: std::slice::from_ref(&layer),
            master_effects,
            master_effect_groups: &[],
            master_trigger_count: 0,
            tonemap: TonemapSettings {
                exposure: 1.0,
                mode: TonemapMode::SceneLinear,
                paper_white_nits: 200.0,
                max_display_nits: 10_000.0,
                curve,
            },
            led_exit_index: -1,
            led_composite_size: (1, 1),
            output_width: WIDTH,
            output_height: HEIGHT,
            occluded_layers: &[],
            render_skip: &[],
        };

        let mut encoder = device.create_encoder("scene-linear-presentation-render");
        {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            compositor.render(&mut gpu, &frame);
        }
        encoder.commit_and_wait_completed();
        let hdr = first_pixel(&readback_raw_halves(
            &device,
            compositor.output_texture(),
            WIDTH,
            HEIGHT,
        ));

        let target = RenderTarget::new(&device, WIDTH, HEIGHT, UI_FORMAT, "scene-presentation-target");
        let pipeline = PresentationPipeline::new(&device);
        let plan = DisplayPlan::new(DisplayCapabilities::sdr(), Some(curve));
        let mut encoder = device.create_encoder("scene-linear-presentation-map");
        pipeline.encode(
            &mut encoder,
            LinearSceneFrame::new(compositor.output_texture()).expect("SceneLinear output"),
            LinearPresentationTarget::new(&target.texture).expect("presentation target"),
            plan,
            (0.0, 0.0, WIDTH as f32, HEIGHT as f32),
            GpuLoadAction::Clear,
        );
        encoder.commit_and_wait_completed();
        let mapped = first_pixel(&readback_raw_halves(&device, &target.texture, WIDTH, HEIGHT));
        RenderSample { hdr, mapped }
    }

    fn narkowicz(value: f32) -> f32 {
        let mapped = (value * (2.51 * value + 0.03))
            / (value * (2.43 * value + 0.59) + 0.14);
        mapped.clamp(0.0, 1.0)
    }

    #[test]
    fn presentation_curves_are_distinct_after_scene_linear_compositor() {
        let source = [2.0, 0.7, 0.1];
        let curves = [
            TonemapCurve::AcesNarkowicz,
            TonemapCurve::AcesHill,
            TonemapCurve::Agx,
            TonemapCurve::KhronosPbrNeutral,
        ];
        let outputs = curves
            .into_iter()
            .map(|curve| render_sample(source, curve, MasterEffect::None).mapped)
            .collect::<Vec<_>>();
        for (index, output) in outputs.iter().enumerate() {
            assert!(output[..3].iter().all(|value| value.is_finite() && (0.0..=1.0).contains(value)));
            assert_eq!(output[3], 1.0);
            for other in outputs.iter().skip(index + 1) {
                let distance = output[..3]
                    .iter()
                    .zip(&other[..3])
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f32::max);
                assert!(distance > 0.01, "curves are not distinct: {output:?} vs {other:?}");
            }
        }
    }

    #[test]
    fn presentation_maps_after_master_invert_and_preserves_alpha() {
        let source = [0.2, 0.4, 0.7];
        let sample = render_sample(source, TonemapCurve::AcesNarkowicz, MasterEffect::Invert);
        let expected_hdr = source.map(|value| 1.0 - value);
        let expected_sdr = expected_hdr.map(narkowicz);
        for channel in 0..3 {
            assert!((sample.hdr[channel] - expected_hdr[channel]).abs() < 0.02);
            assert!((sample.mapped[channel] - expected_sdr[channel]).abs() < 0.02);
        }
        assert_eq!(sample.hdr[3], 1.0);
        assert_eq!(sample.mapped[3], 1.0);
    }

    #[test]
    fn presentation_maps_hdr_after_master_gain() {
        let source = [4.0, 2.0, 0.5];
        let sample = render_sample(source, TonemapCurve::AcesNarkowicz, MasterEffect::ColorGradeGain);
        let expected_hdr = source.map(|value| value * 2.0);
        assert!(expected_hdr[0] > 1.0);
        assert!(sample.hdr[0] > 7.9, "master gain lost HDR headroom: {:?}", sample.hdr);
        assert!((sample.mapped[0] - narkowicz(expected_hdr[0])).abs() < 0.02);
        assert!(sample.mapped[..3].iter().all(|value| value.is_finite() && *value <= 1.0));
    }
