use super::*;
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use manifold_core::audio_visual::AudioVisualRegistry;
use manifold_core::params::ParamManifest;
use manifold_core::{AudioSendId, PresetTypeId};

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
        let output_resource = plan
            .steps()
            .iter()
            .find(|step| step.node == final_node)
            .and_then(|step| step.inputs.iter().find(|(name, _)| *name == "in"))
            .map(|(_, resource)| *resource)
            .expect("final input");
        let (width, height) = (input.width, input.height);
        let source_target = RenderTarget::new(device, width, height, FMT, "audio-proof-input");
        let mut encoder = device.create_encoder("audio-proof-copy-input");
        encoder.copy_texture_to_texture(input, &source_target.texture, width, height, 1);
        encoder.commit_and_wait_completed();
        let mut backend = MetalBackend::new(device.clone(), width, height, FMT);
        crate::node_graph::pre_allocate_resources(&graph, &plan, device, &mut backend)
            .expect("audio graph resources allocate as in production");
        backend.pre_bind_texture_2d(source, source_target);
        let output = backend.pre_bind_texture_2d(
            output_resource,
            RenderTarget::new(device, width, height, FMT, "audio-proof-output"),
        );
        Self {
            graph,
            plan,
            executor: Executor::new(Box::new(backend)),
            output,
            width,
            height,
        }
    }

    fn render(&mut self, device: &GpuDevice, audio: &AudioVisualRegistry, frame: u32) -> Vec<f32> {
        let mut encoder = device.create_encoder("audio-proof-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
            gpu.audio_visuals = Some(audio);
            let mut time = frame_time();
            time.frame_count = i64::from(frame);
            self.executor
                .execute_frame_with_gpu(&mut self.graph, &self.plan, time, &mut gpu);
        }
        encoder.commit_and_wait_completed();
        let texture = self
            .executor
            .backend()
            .texture_2d(self.output)
            .expect("output retained");
        crate::headless_readback::readback_raw_halves(device, texture, self.width, self.height)
            .chunks_exact(2)
            .map(|v| f16::from_bits(u16::from_le_bytes([v[0], v[1]])).to_f32())
            .collect()
    }
}

fn input_texture(device: &GpuDevice, width: u32, height: u32, values: &[[f32; 4]]) -> GpuTexture {
    let texture = device.create_texture(&GpuTextureDesc {
        width,
        height,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "audio-proof-source",
        mip_levels: 1,
    });
    let pixels: Vec<u16> = values
        .iter()
        .flat_map(|pixel| pixel.iter().map(|v| f16::from_f32(*v).to_bits()))
        .collect();
    device.upload_texture(&texture, bytemuck::cast_slice(&pixels));
    texture
}

fn compare_pixels(actual: &[f32], expected: &[f32], tolerance: f32) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            a.is_finite() && (a - b).abs() <= tolerance,
            "channel {i}: {a} != {b}"
        );
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
    let input = input_texture(
        &device,
        4,
        1,
        &[
            [1.0, 1.0, 1.0, 0.3],
            [0.1, 0.1, 0.1, 0.3],
            [0.01, 0.01, 0.01, 0.3],
            [0.0, 0.0, 0.0, 0.3],
        ],
    );
    let registry = PrimitiveRegistry::with_builtin();
    let fused_view =
        super::super::install::fuse_generator_view(&def, &registry).expect("numeric chain fuses");
    assert!(
        fused_view
            .def
            .nodes
            .iter()
            .any(|node| node.type_id == "node.wgsl_compute")
    );
    let empty = AudioVisualRegistry::new();
    let mut raw = AudioGraph::new(&device.arc(), def, &input);
    let mut optimized = AudioGraph::new(&device.arc(), (*fused_view.def).clone(), &input);
    let a = raw.render(&device, &empty, 0);
    let b = optimized.render(&device, &empty, 0);
    let expected: Vec<f32> = [0.0, -0.2, -0.4, -0.6]
        .into_iter()
        .flat_map(|value| [value, value, value, 0.3])
        .collect();
    compare_pixels(&a, &expected, 0.001);
    compare_pixels(&b, &expected, 0.001);
}

fn context(width: u32, height: u32, frame: u32) -> PresetContext {
    PresetContext {
        time: f64::from(frame) / 60.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 1,
        is_clip_level: false,
        frame_count: i64::from(frame),
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn render_generator(
    runtime: &mut PresetRuntime,
    device: &GpuDevice,
    target: &GpuTexture,
    audio: &AudioVisualRegistry,
    frame: u32,
) -> Vec<f32> {
    let mut encoder = device.create_encoder("audio-generator-proof");
    {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, device);
        gpu.audio_visuals = Some(audio);
        runtime.render(
            &mut gpu,
            target,
            &context(target.width, target.height, frame),
            &ParamManifest::default(),
        );
    }
    encoder.commit_and_wait_completed();
    crate::headless_readback::readback_raw_halves(device, target, target.width, target.height)
        .chunks_exact(2)
        .map(|v| f16::from_bits(u16::from_le_bytes([v[0], v[1]])).to_f32())
        .collect()
}

fn audio_fixture() -> (AudioSendId, AudioVisualRegistry) {
    let source = AudioSendId::new("audio-proof");
    let mut audio = AudioVisualRegistry::new();
    audio.set_first_send(Some(&source));
    audio.ensure(&source, 48_000, 64, 256);
    let wave: Vec<f32> = (0..12_000)
        .map(|i| 0.6 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin())
        .collect();
    audio.feed_waveform(&source, &wave);
    let mut column = [0.0; 64];
    for t in 0..470 {
        column.fill(0.0);
        column[8 + (t / 8) % 48] = 0.3;
        audio.feed_spectrum(&source, &column);
    }
    (source, audio)
}

#[test]
fn audio_visual_generators_render_live_sources_and_fuse_on_portrait_canvas() {
    let device = crate::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let (width, height) = (1080, 1920);
    let target = RenderTarget::new(&device, width, height, FMT, "audio-generator-target");
    let (source, audio) = audio_fixture();
    for name in ["Oscilloscope", "Spectrogram"] {
        let def = crate::node_graph::bundled_preset_def(&PresetTypeId::new(name))
            .unwrap()
            .clone();
        assert!(
            def.nodes
                .iter()
                .any(|node| node.type_id == "system.generator_input")
        );
        assert!(!def.nodes.iter().any(|node| node.type_id == "system.source"));
        let fused_view = super::super::install::fuse_generator_view(&def, &primitives);
        let build = |def| {
            PresetRuntime::from_def_with_device(
                def,
                &primitives,
                device.arc(),
                width,
                height,
                FMT,
                None,
            )
            .expect("production generator builds")
        };
        let mut raw = build(def);
        let mut optimized = fused_view.map(|view| build((*view.def).clone()));
        let pixels = render_generator(&mut raw, &device, &target.texture, &audio, 0);
        if let Some(optimized) = optimized.as_mut() {
            let optimized_pixels = render_generator(optimized, &device, &target.texture, &audio, 0);
            compare_pixels(&pixels, &optimized_pixels, 0.015);
        } else {
            // Array IO and rasterization are boundaries; removing the dry/wet
            // mix leaves Oscilloscope without a multi-node texture region.
            assert_eq!(
                name, "Oscilloscope",
                "Spectrogram must retain texture fusion"
            );
        }
        assert!(
            pixels
                .chunks_exact(4)
                .filter(|p| p[3] > 0.1 && p[..3].iter().any(|v| *v > 0.1))
                .count()
                > 100,
            "{name} must produce visible audio without an input image"
        );
        if name == "Spectrogram" {
            assert!(
                pixels.chunks_exact(4).all(|p| (p[3] - 1.0).abs() < 0.001),
                "spectrum covers the portrait canvas"
            );
        }
        let source_node = raw
            .graph
            .nodes()
            .find(|node| {
                matches!(
                    node.node.type_id().as_str(),
                    "node.audio_waveform" | "node.audio_spectrum"
                )
            })
            .unwrap()
            .id;
        raw.graph
            .set_param(
                source_node,
                "send",
                ParamValue::String(std::sync::Arc::new("missing".into())),
            )
            .unwrap();
        let missing = render_generator(&mut raw, &device, &target.texture, &audio, 1);
        assert!(
            pixels
                .iter()
                .zip(&missing)
                .any(|(a, b)| (a - b).abs() > 0.05),
            "{name} source selection must change the image"
        );
        raw.graph
            .set_param(
                source_node,
                "send",
                ParamValue::String(std::sync::Arc::new(String::new())),
            )
            .unwrap();
        let mut silence = AudioVisualRegistry::new();
        silence.set_first_send(Some(&source));
        silence.ensure(&source, 48_000, 64, 256);
        let quiet = render_generator(&mut raw, &device, &target.texture, &silence, 2);
        compare_pixels(&quiet, &missing, 0.001);
        let restored = render_generator(&mut raw, &device, &target.texture, &audio, 3);
        compare_pixels(&restored, &pixels, 0.001);
        raw.resize(&device, 640, 360);
        let resized = RenderTarget::new(&device, 640, 360, FMT, "audio-generator-resized");
        let pixels = render_generator(&mut raw, &device, &resized.texture, &audio, 4);
        assert!(pixels.iter().all(|v| v.is_finite()));
        assert!(
            pixels
                .chunks_exact(4)
                .any(|p| p[3] > 0.1 && p[..3].iter().any(|v| *v > 0.1))
        );
    }
}

#[test]
fn audio_visual_spectrum_chain_keeps_fixed_source_size_on_portrait_canvas() {
    use crate::preset_runtime::ChainBuildInputs;
    let device = crate::test_device();
    let primitives = PrimitiveRegistry::with_builtin();
    let (width, height) = (1080, 1920);
    let mut effect =
        manifold_core::preset_definition_registry::create_default(&PresetTypeId::INVERT_COLORS);
    effect.graph = Some(serde_json::from_value(serde_json::json!({
        "version":2, "name":"Audio fixed-size regression",
        "nodes":[
            {"id":0,"nodeId":"source","typeId":"system.source"},
            {"id":1,"nodeId":"spectrum","typeId":"node.audio_spectrum"},
            {"id":2,"nodeId":"mix","typeId":"node.mix","params":{"amount":{"type":"Float","value":1.0}},"outputCanvasScales":{"out":[1,1]}},
            {"id":3,"nodeId":"output","typeId":"system.final_output"}
        ],
        "wires":[
            {"fromNode":0,"fromPort":"out","toNode":2,"toPort":"a"},
            {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"b"},
            {"fromNode":2,"fromPort":"out","toNode":3,"toPort":"in"}
        ]
    })).unwrap());
    let mut metadata = crate::node_graph::bundled_preset_def(&PresetTypeId::INVERT_COLORS)
        .unwrap()
        .preset_metadata
        .clone()
        .unwrap();
    metadata.params.clear();
    metadata.bindings.clear();
    effect.graph.as_mut().unwrap().preset_metadata = Some(metadata);
    effect.graph_version += 1;
    effect.graph_structure_version += 1;
    let effects = [effect];
    let mut runtime = PresetRuntime::try_build(
        ChainBuildInputs {
            effects: &effects,
            groups: &[],
            primitives: &primitives,
            device: &device,
            pool: None,
            width,
            height,
            preview_effect: Some(&effects[0].id),
        },
        None,
    )
    .expect("spectrum effect chain builds");
    let input = RenderTarget::new(&device, width, height, FMT, "audio-chain-input");
    let (_, audio) = audio_fixture();
    let mut encoder = device.create_encoder("audio-chain-regression");
    {
        let mut gpu = RendererGpuEncoder::new(&mut encoder, &device);
        gpu.audio_visuals = Some(&audio);
        gpu.clear_texture(&input.texture, 0.0, 0.0, 0.0, 1.0);
        runtime
            .run(
                &mut gpu,
                &input.texture,
                &effects,
                &[],
                &context(width, height, 0),
            )
            .unwrap();
    }
    encoder.commit_and_wait_completed();
    let output = runtime.output_texture().unwrap();
    assert_eq!((output.width, output.height), (width, height));
    let pixels = crate::headless_readback::readback_raw_halves(&device, output, width, height);
    assert!(
        pixels
            .chunks_exact(8)
            .all(|p| f16::from_bits(u16::from_le_bytes([p[6], p[7]])).to_f32() == 1.0),
        "sampling must cover the entire output instead of cropping the fixed source"
    );
}
