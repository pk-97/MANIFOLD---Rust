//! `node.clamp` — saturate a texture's RGB channels to a
//! configurable range. Alpha passes through.
//!
//! The texture-side counterpart of `node.array_math` op `Clamp01`: any
//! per-pixel math chain that can produce out-of-range values needs a
//! clamp before consumers that expect bounded input (`node.power`
//! with non-integer exponent, LUT lookups, displacement scales). Defaults
//! to `[0, 1]` — the standard `saturate()` shape from shading code.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ClampUniforms {
    min: f32,
    max: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: ClampTexture,
    type_id: "node.clamp",
    purpose: "Per-pixel clamp on RGB: out.rgb = clamp(in.rgb, min, max). Alpha passes through unchanged. The saturate() atom: pair after scale_offset_texture / power_texture / trig_texture / any chain that produces unbounded output, before LUT lookups / pow with fractional exponent / displacement scales that need a defined input range. Defaults to [0, 1].",
    inputs: {
        in: Texture2D required,
        min: ScalarF32 optional,
        max: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("min"),
            label: "Min",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max"),
            label: "Max",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Both min and max are port-shadows-param so a control wire (LFO, audio bridge) can modulate the clamp range. When min > max the WGSL clamp returns min for all inputs — set sensibly. For one-sided clamps use `min=-INF` (or a very negative value) or `max=INF` (or a very large value). The texture-side sibling of `node.array_math` op `Clamp01`.",
    examples: [],
    picker: { label: "Clamp", category: Atom },
    summary: "Holds every colour between a low and high limit so nothing goes darker or brighter than you set. The tidy-up step after a math node.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["clamp", "clamp texture", "saturate", "limit"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/clamp_texture_body.wgsl"),
}

impl Primitive for ClampTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let min = read("min", 0.0);
        let max = read("max", 1.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.clamp standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.clamp",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ClampUniforms {
            min,
            max,
            _pad0: 0.0,
            _pad1: 0.0,
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
            "node.clamp",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::ClampTexture;
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

    fn run_clamp_at(rgba: [f32; 4], min: f32, max: f32) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(ClampTexture::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "min", ParamValue::Float(min)).unwrap();
        g.set_param(node, "max", ParamValue::Float(max)).unwrap();
        g.connect((src, "out"), (node, "in")).unwrap();
        g.connect((node, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "clamp-src");
        let out_target = RenderTarget::new(&device, w, h, format, "clamp-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [
                rgba[0] as f64,
                rgba[1] as f64,
                rgba[2] as f64,
                rgba[3] as f64,
            ],
            "clamp-src-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("clamp-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("clamp output texture retained");
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut readback_enc = device.create_encoder("clamp-readback");
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
    fn in_range_values_pass_through() {
        let src = [0.3_f32, 0.6, 0.2, 1.0];
        let out = run_clamp_at(src, 0.0, 1.0);
        for c in 0..3 {
            assert!(
                (out[c] - src[c]).abs() < 0.01,
                "ch {c}: {} != {}",
                out[c],
                src[c]
            );
        }
        assert!((out[3] - src[3]).abs() < 0.01, "alpha pass-through");
    }

    #[test]
    fn above_max_clamps_to_max() {
        let src = [1.5_f32, 2.0, 3.0, 1.0];
        let out = run_clamp_at(src, 0.0, 1.0);
        for (c, &v) in out.iter().take(3).enumerate() {
            assert!(v <= 1.01, "ch {c} above max: {v}");
            assert!(v >= 0.99, "ch {c} clamped to max: {v}");
        }
    }

    #[test]
    fn below_min_clamps_to_min() {
        let src = [-0.5_f32, -1.0, 0.2, 1.0];
        let out = run_clamp_at(src, 0.0, 1.0);
        assert!(out[0] <= 0.01, "R clamped to 0: {}", out[0]);
        assert!(out[1] <= 0.01, "G clamped to 0: {}", out[1]);
        assert!((out[2] - 0.2).abs() < 0.01, "B passes through: {}", out[2]);
    }

    #[test]
    fn custom_range_clamps_correctly() {
        let src = [0.1_f32, 0.5, 0.9, 1.0];
        let out = run_clamp_at(src, 0.3, 0.7);
        assert!((out[0] - 0.3).abs() < 0.01, "R clamped up to min");
        assert!((out[1] - 0.5).abs() < 0.01, "G in range");
        assert!((out[2] - 0.7).abs() < 0.01, "B clamped down to max");
    }
}
