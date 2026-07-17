//! `node.exposure` — multiply the input texture's RGB channels by a
//! scalar `gain` value. Alpha passes through unchanged.
//!
//! Smallest possible scalar-driven texture primitive: drop one on
//! top of any source, wire an LFO/BeatGate/audio bridge into the
//! `gain` port, and the source flashes/breathes/pulses on the
//! incoming signal. Strobe's Opacity-mode is just
//! `Source → Gain(gain = 1 - beat_gate)`; Strobe's Gain-mode is
//! `Source → Gain(gain = 1 + 2 * beat_gate)`.
//!
//! Distinct from `node.brightness`, which (when implemented) extracts
//! Rec. 709 luminance from RGB to produce a grayscale image — a
//! channel reshape, not a magnitude change.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GainUniforms {
    gain: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Gain,
    type_id: "node.exposure",
    purpose: "Multiply the input texture's RGB by a scalar gain. Alpha passes through unchanged. The `gain` input port is the standard control-wire shadow of the `gain` param — wire any scalar source (LFO, BeatGate, Luminance, …) to make the gain react in real time.",
    inputs: {
        in: Texture2D required,
        gain: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("gain"),
            label: "Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Wire wins over the param — same convention as `node.wet_dry`. Range above 1.0 brightens the image; below 1.0 darkens; 0.0 produces black. For Strobe-style modes: Opacity = Gain(1 - beat_gate), Gain-mode = Gain(1 + 2*beat_gate).",
    examples: ["preset.strobe"],
    picker: { label: "Exposure", category: Atom },
    summary: "Brightens or darkens the whole image by multiplying every colour. Above 1 brightens, below 1 darkens, and 0 is black.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["gain", "brightness", "exposure", "Level TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/gain_body.wgsl"),
}

impl Primitive for Gain {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Wire wins, param is the fallback. Same convention as
        // `node.wet_dry`'s scalar input.
        let gain = match ctx.inputs.scalar("gain") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("gain") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.exposure standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.exposure",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = GainUniforms {
            gain,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.exposure",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::Gain;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId, Source,
    };
    use crate::render_target::RenderTarget;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    fn run_gain_at(rgba: [f32; 4], gain: f32) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let gain_node = g.add_node(Box::new(Gain::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(gain_node, "gain", ParamValue::Float(gain)).unwrap();
        g.connect((src, "out"), (gain_node, "in")).unwrap();
        g.connect((gain_node, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, gain_node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "gain-src");
        let out_target = RenderTarget::new(&device, w, h, format, "gain-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [rgba[0] as f64, rgba[1] as f64, rgba[2] as f64, rgba[3] as f64],
            "gain-src-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("gain-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("gain output texture retained");
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut readback_enc = device.create_encoder("gain-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared readback");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(pixels[0]).to_f32(),
            f16::from_bits(pixels[1]).to_f32(),
            f16::from_bits(pixels[2]).to_f32(),
            f16::from_bits(pixels[3]).to_f32(),
        ]
    }

    #[test]
    fn gain_one_returns_source_unchanged() {
        let src = [0.3_f32, 0.6, 0.2, 1.0];
        let out = run_gain_at(src, 1.0);
        for c in 0..4 {
            assert!((out[c] - src[c]).abs() < 0.01, "ch {c}: {} != {}", out[c], src[c]);
        }
    }

    #[test]
    fn gain_zero_returns_black_with_alpha_preserved() {
        let src = [0.3_f32, 0.6, 0.2, 0.8];
        let out = run_gain_at(src, 0.0);
        assert!(out[0] < 0.01, "R should be 0");
        assert!(out[1] < 0.01, "G should be 0");
        assert!(out[2] < 0.01, "B should be 0");
        assert!((out[3] - src[3]).abs() < 0.01, "alpha should pass through");
    }

    #[test]
    fn gain_two_doubles_rgb() {
        let src = [0.2_f32, 0.3, 0.4, 1.0];
        let out = run_gain_at(src, 2.0);
        for c in 0..3 {
            assert!((out[c] - 2.0 * src[c]).abs() < 0.01, "ch {c}: {} != {}", out[c], 2.0 * src[c]);
        }
        assert!((out[3] - src[3]).abs() < 0.01);
    }

    /// Wire wins over param — wire a Value(0.5) into the `gain` port
    /// while the param is set to 2.0. The output should reflect 0.5
    /// (halved) not 2.0 (doubled).
    #[test]
    fn wired_gain_overrides_param() {
        use crate::node_graph::primitives::Value;

        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;
        let src_rgba = [0.4_f32, 0.6, 0.8, 1.0];

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let val = g.add_node(Box::new(Value::new()));
        let gain_node = g.add_node(Box::new(Gain::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        // Param says 2.0 (would double). Wire from Value(0.5) overrides.
        g.set_param(gain_node, "gain", ParamValue::Float(2.0)).unwrap();
        g.set_param(val, "value", ParamValue::Float(0.5)).unwrap();
        g.connect((src, "out"), (gain_node, "in")).unwrap();
        g.connect((val, "out"), (gain_node, "gain")).unwrap();
        g.connect((gain_node, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, gain_node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "gain-wired-src");
        let out_target = RenderTarget::new(&device, w, h, format, "gain-wired-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [src_rgba[0] as f64, src_rgba[1] as f64, src_rgba[2] as f64, src_rgba[3] as f64],
            "gain-wired-src-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("gain-wired-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec.backend().texture_2d(out_slot).expect("retained");
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut readback_enc = device.create_encoder("gain-wired-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let out_rgba = [
            f16::from_bits(pixels[0]).to_f32(),
            f16::from_bits(pixels[1]).to_f32(),
            f16::from_bits(pixels[2]).to_f32(),
        ];
        // 0.5× the source, not 2.0×.
        for c in 0..3 {
            let expected = 0.5 * src_rgba[c];
            assert!(
                (out_rgba[c] - expected).abs() < 0.01,
                "ch {c}: expected {expected} (wire 0.5 × src), got {} (would be {} at param 2.0)",
                out_rgba[c],
                2.0 * src_rgba[c],
            );
        }
    }
}
