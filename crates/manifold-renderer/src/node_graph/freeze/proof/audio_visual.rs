use super::*;
use manifold_core::audio_visual::AudioVisualRegistry;
use manifold_core::{AudioSendId, PresetTypeId};
use manifold_core::effect_graph_def::SerializedParamValue;

struct AudioGraph {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    output: crate::node_graph::Slot,
    width: u32,
    height: u32,
}

impl AudioGraph {
    fn new(device: &std::sync::Arc<GpuDevice>, def: EffectGraphDef, input: &GpuTexture) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let graph = def.into_graph(&registry).expect("audio graph loads");
        let plan = compile(&graph).expect("audio graph compiles");
        let source = resource_for_output(&plan, find_node(&graph, "system.source"), "out");
        let final_node = find_node(&graph, "system.final_output");
        let output_resource = plan.steps().iter().find(|step| step.node == final_node)
            .and_then(|step| step.inputs.iter().find(|(name, _)| *name == "in"))
            .map(|(_, resource)| *resource).expect("final input");
        let (width, height) = (input.width, input.height);
        let source_target = RenderTarget::new(device, width, height, FMT, "audio-proof-input");
        let mut encoder = device.create_encoder("audio-proof-copy-input");
        encoder.copy_texture_to_texture(input, &source_target.texture, width, height, 1);
        encoder.commit_and_wait_completed();
        let mut backend = MetalBackend::new(device.clone(), width, height, FMT);
        crate::node_graph::pre_allocate_resources(&graph, &plan, device, &mut backend)
            .expect("audio graph resources allocate as in production");
        backend.pre_bind_texture_2d(source, source_target);
        let output = backend.pre_bind_texture_2d(output_resource,
            RenderTarget::new(device, width, height, FMT, "audio-proof-output"));
        Self { graph, plan, executor: Executor::new(Box::new(backend)), output, width, height }
    }

    fn render(&mut self, device: &GpuDevice, audio: &AudioVisualRegistry, frame: u32) -> Vec<f32> {
        let mut encoder = device.create_encoder("audio-proof-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
            gpu.audio_visuals = Some(audio);
            let mut time = frame_time();
            time.frame_count = i64::from(frame);
            self.executor.execute_frame_with_gpu(&mut self.graph, &self.plan, time, &mut gpu);
        }
        encoder.commit_and_wait_completed();
        let texture = self.executor.backend().texture_2d(self.output).expect("output retained");
        crate::headless_readback::readback_raw_halves(device, texture, self.width, self.height)
            .chunks_exact(2).map(|v| f16::from_bits(u16::from_le_bytes([v[0], v[1]])).to_f32()).collect()
    }
}

fn input_texture(device: &GpuDevice, width: u32, height: u32, values: &[[f32; 4]]) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width, height, depth: 1, format: FMT, dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label: "audio-proof-source", mip_levels: 1,
    });
    let pixels: Vec<u16> = values.iter().flat_map(|pixel| pixel.iter().map(|v| f16::from_f32(*v).to_bits())).collect();
    device.upload_texture(&texture, bytemuck::cast_slice(&pixels));
    texture
}

fn compare_pixels(actual: &[f32], expected: &[f32], tolerance: f32) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(a.is_finite() && (a - b).abs() <= tolerance, "channel {i}: {a} != {b}");
    }
}

#[test]
fn audio_visual_magnitude_db_matches_math_and_fusion() {
    let device = crate::test_device();
    let def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version":2, "name":"Magnitude proof",
        "nodes":[
            {"id":0,"nodeId":"source","typeId":"system.source"},
            {"id":1,"nodeId":"db","typeId":"node.magnitude_db"},
            {"id":2,"nodeId":"scale","typeId":"node.scale_offset_image","params":{"scale":{"type":"Float","value":0.01},"offset":{"type":"Float","value":0.0}}},
            {"id":3,"nodeId":"output","typeId":"system.final_output"}
        ],
        "wires":[
            {"fromNode":0,"fromPort":"out","toNode":1,"toPort":"in"},
            {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"in"},
            {"fromNode":2,"fromPort":"out","toNode":3,"toPort":"in"}
        ]
    })).unwrap();
    let input = input_texture(&device, 4, 1, &[
        [1.0, 1.0, 1.0, 0.3], [0.1, 0.1, 0.1, 0.3],
        [0.01, 0.01, 0.01, 0.3], [0.0, 0.0, 0.0, 0.3],
    ]);
    let registry = PrimitiveRegistry::with_builtin();
    let fused = super::super::install::fuse_generator_def(&def, &registry).expect("numeric chain fuses");
    assert!(fused.nodes.iter().any(|node| node.type_id == "node.wgsl_compute"));
    let empty = AudioVisualRegistry::new();
    let mut raw = AudioGraph::new(&device.arc(), def, &input);
    let mut optimized = AudioGraph::new(&device.arc(), fused, &input);
    let a = raw.render(&device, &empty, 0);
    let b = optimized.render(&device, &empty, 0);
    let expected: Vec<f32> = [0.0, -0.2, -0.4, -0.6].into_iter()
        .flat_map(|value| [value, value, value, 0.3]).collect();
    compare_pixels(&a, &expected, 0.001);
    compare_pixels(&b, &expected, 0.001);
}

#[test]
fn audio_visual_effects_render_live_sources_mix_and_fuse() {
    let device = crate::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let (width, height) = (1024u32, 512u32);
    let dry_pixel = [0.05, 0.08, 0.1, 0.3];
    let input = input_texture(&device, width, height, &vec![dry_pixel; (width * height) as usize]);
    let dry: Vec<f32> = (0..width * height).flat_map(|_| dry_pixel).collect();
    let source = AudioSendId::new("audio-proof");
    let mut audio = AudioVisualRegistry::new();
    audio.set_first_send(Some(&source));
    audio.ensure(&source, 48_000, 64, 256);
    let wave: Vec<f32> = (0..12_000).map(|i| 0.6 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin()).collect();
    audio.feed_waveform(&source, &wave);
    let mut column = [0.0; 64];
    for t in 0..470 {
        column.fill(0.0);
        column[8 + (t / 8) % 48] = 0.3;
        audio.feed_spectrum(&source, &column);
    }
    let mut proof = image::RgbImage::new(width, height * 2);
    for (row, name) in ["Oscilloscope", "Spectrogram"].into_iter().enumerate() {
        let def = crate::node_graph::bundled_preset_def(&PresetTypeId::new(name)).unwrap().clone();
        let fused = super::super::install::fuse_generator_def(&def, &primitives).expect("visual graph regions fuse");
        let mut raw = AudioGraph::new(&device.arc(), def.clone(), &input);
        let mut optimized = AudioGraph::new(&device.arc(), fused, &input);
        let pixels = raw.render(&device, &audio, 0);
        let optimized_pixels = optimized.render(&device, &audio, 0);
        compare_pixels(&pixels, &optimized_pixels, 0.015);
        assert!(pixels.iter().zip(&dry).filter(|(a,b)| (*a - *b).abs() > 0.03).count() > 100,
            "{name} must produce a visible audio image");
        for pixel in pixels.chunks_exact(4) { assert!((pixel[3] - 0.3).abs() < 0.001); }
        for (i, pixel) in pixels.chunks_exact(4).enumerate() {
            proof.put_pixel(i as u32 % width, row as u32 * height + i as u32 / width,
                image::Rgb(std::array::from_fn(|c| (pixel[c].clamp(0.0, 1.0) * 255.0) as u8)));
        }
        let mut bypass = def.clone();
        let mix = bypass.nodes.iter_mut().find(|node| node.type_id == "node.mix").unwrap();
        mix.params.insert("amount".into(), SerializedParamValue::Float { value: 0.0 });
        let zero = AudioGraph::new(&device.arc(), bypass, &input).render(&device, &audio, 0);
        compare_pixels(&zero, &dry, 0.001);

        // Reuse the same graph/executor: external data must advance even when
        // all node parameters are unchanged. A missing explicit source never
        // falls back to the valid default send.
        let source_node = raw.graph.nodes().find(|node| matches!(node.node.type_id().as_str(),
            "node.audio_waveform" | "node.audio_spectrum")).unwrap().id;
        raw.graph.set_param(source_node, "send", ParamValue::String(std::sync::Arc::new("missing".into()))).unwrap();
        let missing = raw.render(&device, &audio, 1);
        assert!(pixels.iter().zip(&missing).any(|(a,b)| (a-b).abs() > 0.05), "{name} source selection must change the image");
        raw.graph.set_param(source_node, "send", ParamValue::String(std::sync::Arc::new(String::new()))).unwrap();
        let mut silence = AudioVisualRegistry::new();
        silence.set_first_send(Some(&source));
        silence.ensure(&source, 48_000, 64, 256);
        let quiet = raw.render(&device, &silence, 2);
        compare_pixels(&quiet, &missing, 0.001);
        let restored = raw.render(&device, &audio, 3);
        compare_pixels(&restored, &pixels, 0.001);
    }
    let path = std::env::temp_dir().join("manifold-audio-visualizers.png");
    proof.save(&path).unwrap();
    println!("Audio visualizer proof: {}", path.display());
}
