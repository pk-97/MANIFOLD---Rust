//! `node.levels` — fused tone-shaping atom.
//!
//! `out.rgb = pow(clamp(in.rgb * scale + offset, lo, hi), gamma)`, alpha
//! pass-through. Collapses the `scale_offset_texture → clamp_texture →
//! power_texture` cluster (3 dispatches + 2 intermediate WxH textures)
//! into one shader. The same cluster appears in MetallicGlass's height
//! and metallic chains, in Halation's bloom thresholds, and in OilyFluid's
//! hue ramps — anywhere per-channel affine + clamp + gamma sits between
//! atoms. Curated medium-grain primitive: the decomposition is still
//! expressible via the three component atoms when authoring needs to
//! inspect intermediates; `node.levels` is what you reach for once the
//! shape is settled.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LevelsUniforms {
    scale: f32,
    offset: f32,
    lo: f32,
    hi: f32,
    gamma: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Levels,
    type_id: "node.levels",
    purpose: "Fused tone-shape atom: out.rgb = pow(clamp(in.rgb * scale + offset, lo, hi), gamma). Alpha pass-through. Collapses the scale_offset_texture → clamp_texture → power_texture trio (and its bandwidth) into one dispatch. Reach for this wherever a per-channel affine remap, clamp, and gamma all appear together — MetallicGlass height/metallic chains, halation bloom shaping, hue ramps.",
    inputs: {
        in: Texture2D required,
        scale: ScalarF32 optional,
        offset: ScalarF32 optional,
        lo: ScalarF32 optional,
        hi: ScalarF32 optional,
        gamma: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset"),
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("lo"),
            label: "Lo",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("hi"),
            label: "Hi",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("gamma"),
            label: "Gamma",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.01, 16.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "All five inputs are port-shadows-param so a control wire can modulate any axis. Defaults (scale=1, offset=0, lo=0, hi=1, gamma=1) are a saturate. For a passthrough, widen lo/hi past the input range. pow on a negative base is undefined — the shader maxes against 0 before pow so a misconfigured `lo < 0` with a fractional gamma still produces defined output. Composes with itself: a `levels → levels` chain in JSON is two dispatches, equivalent to the legacy MetallicGlass's `(feedback_luma * 0.7 + 0.3) * 1.8 - 0.25` chain expressed as one levels per stage (or pre-multiplied into one levels — `scale = 0.7 * 1.8 = 1.26`, `offset = 0.3 * 1.8 - 0.25 = 0.29`).",
    examples: ["preset.generator.metallic_glass"],
    picker: { label: "Levels", category: Atom },
    summary: "Reshapes brightness in one step with scale, offset, a clamp, and gamma. A compact way to lift shadows, crush highlights, or set black and white points.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["levels", "gamma", "curves", "Level TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/levels_body.wgsl"),
}

impl Primitive for Levels {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = ctx.scalar_or_param("scale", 1.0);
        let offset = ctx.scalar_or_param("offset", 0.0);
        let lo = ctx.scalar_or_param("lo", 0.0);
        let hi = ctx.scalar_or_param("hi", 1.0);
        let gamma = ctx.scalar_or_param("gamma", 1.0);

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
            // Single-source: standalone kernel generated from the same
            // `wgsl_body` the fusion codegen chains. levels.wgsl is retained as
            // the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.levels standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.levels",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = LevelsUniforms {
            scale,
            offset,
            lo,
            hi,
            gamma,
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
            "node.levels",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::Levels;
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

    fn run_levels_at(
        rgba: [f32; 4],
        scale: f32,
        offset: f32,
        lo: f32,
        hi: f32,
        gamma: f32,
    ) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(Levels::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "scale", ParamValue::Float(scale)).unwrap();
        g.set_param(node, "offset", ParamValue::Float(offset)).unwrap();
        g.set_param(node, "lo", ParamValue::Float(lo)).unwrap();
        g.set_param(node, "hi", ParamValue::Float(hi)).unwrap();
        g.set_param(node, "gamma", ParamValue::Float(gamma)).unwrap();
        g.connect((src, "out"), (node, "in")).unwrap();
        g.connect((node, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "levels-src");
        let out_target = RenderTarget::new(&device, w, h, format, "levels-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [
                rgba[0] as f64,
                rgba[1] as f64,
                rgba[2] as f64,
                rgba[3] as f64,
            ],
            "levels-src-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("levels-frame");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("levels output retained");
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut readback_enc = device.create_encoder("levels-readback");
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

    /// Default-shaped saturate (`scale=1, offset=0, lo=0, hi=1, gamma=1`)
    /// passes in-range inputs through unchanged and the alpha untouched.
    #[test]
    fn identity_saturate_passes_in_range_through() {
        let src = [0.3_f32, 0.6, 0.2, 0.7];
        let out = run_levels_at(src, 1.0, 0.0, 0.0, 1.0, 1.0);
        for (c, (&o, &s)) in out.iter().zip(src.iter()).take(3).enumerate() {
            assert!((o - s).abs() < 0.01, "ch {c}: {o} != {s}");
        }
        assert!((out[3] - src[3]).abs() < 0.01, "alpha pass-through");
    }

    /// The MetallicGlass height chain reduces to scale=1.26, offset=0.29,
    /// lo=0, hi=1, gamma=0.8. Verify the analytic value for a mid input.
    #[test]
    fn metallic_glass_height_shape() {
        // feedback_luma = 0.5  → (0.5*1.26 + 0.29) = 0.92 → clamp [0,1] = 0.92 → pow 0.8 ≈ 0.9354
        let out = run_levels_at([0.5, 0.5, 0.5, 1.0], 1.26, 0.29, 0.0, 1.0, 0.8);
        let expected = 0.92_f32.powf(0.8);
        for (c, &v) in out.iter().take(3).enumerate() {
            assert!(
                (v - expected).abs() < 0.02,
                "ch {c}: {v} != {expected} (height chain)",
            );
        }
    }

    /// The MetallicGlass metallic-invert+clamp+pow shape: scale=-1,
    /// offset=1, lo=0, hi=1, gamma=1.5.
    #[test]
    fn metallic_glass_metallic_shape() {
        // edge_clamped = 0.4 → 1 - 0.4 = 0.6 → clamp = 0.6 → pow 1.5 ≈ 0.4648
        let out = run_levels_at([0.4, 0.4, 0.4, 1.0], -1.0, 1.0, 0.0, 1.0, 1.5);
        let expected = 0.6_f32.powf(1.5);
        for (c, &v) in out.iter().take(3).enumerate() {
            assert!(
                (v - expected).abs() < 0.02,
                "ch {c}: {v} != {expected} (metallic chain)",
            );
        }
    }

    /// Out-of-range inputs are clamped before the gamma curve fires —
    /// guards against the historical `pow(negative)` undefined-result class.
    #[test]
    fn negative_input_with_fractional_gamma_is_defined() {
        // Input -0.5 → clamp to lo=0 → pow(0, 0.5) = 0
        let out = run_levels_at([-0.5, -0.5, -0.5, 1.0], 1.0, 0.0, 0.0, 1.0, 0.5);
        for (c, &v) in out.iter().take(3).enumerate() {
            assert!(v.abs() < 0.01, "ch {c}: {v} should be 0");
        }
    }
}
