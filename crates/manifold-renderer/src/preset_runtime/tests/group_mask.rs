use super::*;
use crate::preset_context::PresetContext;
use crate::render_target::RenderTarget;
use half::f16;

fn fixture(mask_last: bool) -> (Vec<PresetInstance>, Vec<EffectGroup>) {
    let mut group = EffectGroup::new("Masked".into());
    let mut mask = manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    let mut wet = mask.duplicated();
    mask.group_id = Some(group.id.clone());
    wet.group_id = Some(group.id.clone());
    group.mask_effect_id = Some(mask.id.clone());
    let effects = if mask_last { vec![wet, mask] } else { vec![mask, wet] };
    (effects, vec![group])
}

#[test]
fn group_mask_reads_dry_input_and_updates_without_rebuild() {
    let device = crate::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    for mask_last in [false, true] {
        let (mut effects, mut groups) = fixture(mask_last);
        let mut runtime = PresetRuntime::try_build(ChainBuildInputs {
            effects: &effects, groups: &groups, primitives: &primitives,
            device: &device, pool: None, width: 16, height: 16, preview_effect: None,
        }, None).expect("masked group builds");
        let topology = runtime.topology_hash;
        let input = RenderTarget::new(&device, 16, 16, GRAPH_FORMAT, "group-mask-input");
        let ctx = PresetContext { time: 0.0, beat: 0.0, dt: 1.0 / 60.0,
            width: 16, height: 16, output_width: 16, output_height: 16,
            aspect: 1.0, owner_key: 0, is_clip_level: false, frame_count: 0,
            anim_progress: 0.0, trigger_count: 0 };
        // The inverted dry image is itself the mask: coverage .8 at amount 1,
        // .2 at amount 0. If the mask reads the wet branch this assertion fails.
        for (mask_amount, wet_dry, expected) in [(1.0, 1.0, 0.68), (0.0, 1.0, 0.32), (1.0, 0.5, 0.44), (1.0, 0.0, 0.2)] {
            effects[usize::from(mask_last)].set_base_param("amount", mask_amount);
            groups[0].wet_dry = wet_dry;
            assert_eq!(compute_topology_hash(&effects, &groups, 0, 0, None), topology);
            let mut encoder = device.create_encoder("group-mask-proof");
            {
                let mut gpu = GpuEncoder::new(&mut encoder, &device);
                gpu.clear_texture(&input.texture, 0.2, 0.2, 0.2, 0.3);
                runtime.run(&mut gpu, &input.texture, &effects, &groups, &ctx).expect("output");
            }
            encoder.commit_and_wait_completed();
            let bytes = crate::headless_readback::readback_raw_halves(&device, runtime.output_texture().unwrap(), 16, 16);
            for pixel in bytes.chunks_exact(8) {
                for (channel, value) in pixel.chunks_exact(2).enumerate() {
                    let actual = f16::from_bits(u16::from_le_bytes([value[0], value[1]])).to_f32();
                    let wanted = if channel == 3 { 0.3 } else { expected };
                    assert!((actual - wanted).abs() < 0.002, "mask_last={mask_last} amount={mask_amount} wet={wet_dry}: channel {channel}: {actual} != {wanted}");
                }
            }
        }
    }
}

#[test]
fn group_mask_disabled_uses_normal_mix_and_missing_member_is_rejected() {
    let device = crate::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let (mut effects, groups) = fixture(false);
    effects[0].enabled = false;
    let build = |effects: &[PresetInstance]| PresetRuntime::try_build(ChainBuildInputs {
        effects, groups: &groups, primitives: &primitives, device: &device,
        pool: None, width: 16, height: 16, preview_effect: None,
    }, None);
    let runtime = build(&effects).expect("disabled mask bypasses masking");
    let mix = runtime.graph.get_node(runtime.group_mix_nodes[0].1).unwrap();
    assert_eq!(mix.node.type_id().as_str(), "node.mix");
    assert!(build(&effects[1..]).is_none(), "missing mask must not render as colour");
}

#[test]
fn group_mask_layer_source_reaches_dispatch_and_survives_reload() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::LayerId;
    let device = crate::test_device();
    let mut group = EffectGroup::new("Sidechain".into());
    let mut mask = manifold_core::preset_definition_registry::create_default(&PresetTypeId::new("MaskLayer"));
    let view = loaded_preset_view_by_id(mask.effect_type()).expect("mask preset registered");
    let mut def = (*view.canonical_def).clone();
    let source = def.nodes.iter_mut().find(|n| n.type_id == "node.layer_source").unwrap();
    source.params.insert("layer".into(), SerializedParamValue::String { value: "source-layer".into() });
    mask.graph = Some(def);
    mask.group_id = Some(group.id.clone());
    group.mask_effect_id = Some(mask.id.clone());
    let mut wet = manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    wet.group_id = Some(group.id.clone());
    let serialized = serde_json::to_string(&(vec![mask, wet], vec![group])).unwrap();
    let (effects, groups): (Vec<PresetInstance>, Vec<EffectGroup>) = serde_json::from_str(&serialized).unwrap();
    let mut project = manifold_core::project::Project::default();
    project.settings.master_effects = effects;
    project.settings.master_effect_groups = Some(groups);
    project.reconcile_param_manifests();
    let mut effects = project.settings.master_effects;
    let groups = project.settings.master_effect_groups.unwrap();
    let input = RenderTarget::new(&device, 16, 16, GRAPH_FORMAT, "mask-dry");
    let feed = RenderTarget::new(&device, 16, 16, GRAPH_FORMAT, "mask-sidechain");
    let mut registry = crate::layer_skin::LayerSkinRegistry::new(&device, GRAPH_FORMAT);
    let mut cache = None;
    let ctx = PresetContext { time: 0.0, beat: 0.0, dt: 1.0 / 60.0,
        width: 16, height: 16, output_width: 16, output_height: 16,
        aspect: 1.0, owner_key: 0, is_clip_level: false, frame_count: 0,
        anim_progress: 0.0, trigger_count: 0 };
    // Brightness, alpha, missing source. Change the channel after reload and
    // on the cached runtime to prove the real binding path remains live.
    for (channel, present, expected) in [(0.0, true, 0.8), (1.0, true, 0.35), (0.0, false, 0.2)] {
        effects[0].set_base_param("channel", channel);
        registry.clear();
        let mut encoder = device.create_encoder("mask-sidechain-proof");
        let output = {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            gpu.clear_texture(&input.texture, 0.2, 0.2, 0.2, 0.3);
            gpu.clear_texture(&feed.texture, 1.0, 1.0, 1.0, 0.25);
            registry.ensure_fallback_cleared(&mut gpu);
            registry.begin_snapshots();
            if present { registry.publish_snapshot(&mut gpu, &LayerId::new("source-layer"), &feed.texture); }
            registry.finish_snapshots();
            crate::chain_dispatch::dispatch_chain(&mut cache, &mut gpu, &input.texture,
                &effects, &groups, &ctx, None, "group-mask-test", false,
                crate::node_graph::RtQuality::default(), &registry).unwrap().clone()
        };
        encoder.commit_and_wait_completed();
        let raw = crate::headless_readback::readback_raw_halves(&device, &output, 16, 16);
        let red = f16::from_bits(u16::from_le_bytes([raw[0], raw[1]])).to_f32();
        assert!((red - expected).abs() < 0.002, "channel={channel} source={present}: {red} != {expected}");
    }
}

#[test]
fn group_mask_circle_moves_over_infrared_without_rebuild() {
    let device = crate::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let mut group = EffectGroup::new("Scanning Infrared".into());
    let mut mask = manifold_core::preset_definition_registry::create_default(&PresetTypeId::new("MaskCircle"));
    mask.set_base_param("size_x", 0.2);
    mask.set_base_param("size_y", 0.35);
    mask.set_base_param("feather", 0.2);
    let mut wet = manifold_core::preset_definition_registry::create_default(&PresetTypeId::INFRARED);
    wet.set_base_param("amount", 1.0);
    wet.set_base_param("palette", 3.0);
    let plain_effects = vec![wet.clone()];
    mask.group_id = Some(group.id.clone());
    wet.group_id = Some(group.id.clone());
    group.mask_effect_id = Some(mask.id.clone());
    let mut effects = vec![mask, wet];
    let groups = vec![group];
    let build = |effects: &[PresetInstance], groups: &[EffectGroup]| {
        PresetRuntime::try_build(ChainBuildInputs {
            effects, groups, primitives: &primitives, device: &device,
            pool: None, width: 64, height: 32, preview_effect: None,
        }, None).unwrap()
    };
    let mut masked = build(&effects, &groups);
    let mut plain = build(&plain_effects, &[]);
    let input = RenderTarget::new(&device, 64, 32, GRAPH_FORMAT, "circle-scan-input");
    let ctx = PresetContext { time: 0.0, beat: 0.0, dt: 1.0 / 60.0,
        width: 64, height: 32, output_width: 64, output_height: 32,
        aspect: 2.0, owner_key: 0, is_clip_level: false, frame_count: 0,
        anim_progress: 0.0, trigger_count: 0 };
    let mut proof = image::RgbImage::new(128, 32);
    for (frame, centre) in [0.25, 0.75].into_iter().enumerate() {
        effects[0].set_base_param("position_x", centre);
        assert_eq!(masked.topology_hash, compute_topology_hash(&effects, &groups, 0, 0, None));
        let mut encoder = device.create_encoder("circle-scan-proof");
        {
            let mut gpu = GpuEncoder::new(&mut encoder, &device);
            gpu.clear_texture(&input.texture, 0.2, 0.2, 0.2, 0.3);
            plain.run(&mut gpu, &input.texture, &plain_effects, &[], &ctx).unwrap();
            masked.run(&mut gpu, &input.texture, &effects, &groups, &ctx).unwrap();
        }
        encoder.commit_and_wait_completed();
        let wet = crate::headless_readback::readback_raw_halves(&device, plain.output_texture().unwrap(), 64, 32);
        let output = crate::headless_readback::readback_raw_halves(&device, masked.output_texture().unwrap(), 64, 32);
        let channel = |bytes: &[u8], x: usize, c: usize| {
            let i = (16 * 64 + x) * 8 + c * 2;
            f16::from_bits(u16::from_le_bytes([bytes[i], bytes[i + 1]])).to_f32()
        };
        let inside = if frame == 0 { 16 } else { 48 };
        let outside = if frame == 0 { 48 } else { 16 };
        assert!((0..3).any(|c| (channel(&wet, inside, c) - 0.2).abs() > 0.05), "Infrared reference must visibly differ from dry input");
        for c in 0..4 {
            assert!((channel(&output, inside, c) - channel(&wet, inside, c)).abs() < 0.002);
            assert!((channel(&output, outside, c) - if c == 3 { 0.3 } else { 0.2 }).abs() < 0.002);
        }
        for (i, pixel) in output.chunks_exact(8).enumerate() {
            let rgb = std::array::from_fn(|c| {
                let value = f16::from_bits(u16::from_le_bytes([pixel[c * 2], pixel[c * 2 + 1]])).to_f32();
                (value.clamp(0.0, 1.0) * 255.0) as u8
            });
            proof.put_pixel((frame * 64 + i % 64) as u32, (i / 64) as u32, image::Rgb(rgb));
        }
    }
    proof.save(std::env::temp_dir().join("manifold-mask-scan.png")).unwrap();
}
