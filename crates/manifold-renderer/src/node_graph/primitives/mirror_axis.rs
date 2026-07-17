//! `node.flip` — sample a texture at UVs mirrored across a line
//! through the center at a given angle.
//!
//! Single-axis 2-fold symmetry: one half of the input is visible, the
//! other half is its mirror image across the axis. Distinct from
//! `node.transform`'s fold modes (axis-aligned both-halves "kaleidoscope"
//! fold) and `node.kaleidoscope` (polar N-segment fold).
//!
//! Math (matches the TD "Mirror TOP at angle" semantics):
//!
//!   1. centered  = uv - 0.5
//!   2. rotated   = R(-angle) · centered
//!   3. folded    = (rotated.x, abs(rotated.y))      // single-axis fold
//!   4. unrotated = R(+angle) · folded
//!   5. sample input at fract(unrotated + 0.5)
//!
//! `angle = 0` mirrors across the horizontal centerline (bottom half =
//! mirror of top). `angle = π/4` mirrors across the +45° diagonal.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorAxisUniforms {
    angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: MirrorAxis,
    type_id: "node.flip",
    purpose: "Sample input at UVs mirrored across a line through center at `angle` radians. Single-axis 2-fold symmetry (one half visible, other half is mirror). Distinct from `node.transform` fold modes (axis-aligned, both halves visible mirrored) and `node.kaleidoscope` (N-segment radial). Use for tilted symmetry overlays, asymmetric kaleidoscope variants, height-map symmetry in shading chains.",
    inputs: {
        in: Texture2D required,
        angle: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("angle"),
            label: "Angle",
            ty: ParamType::Angle,
            default: ParamValue::Float(std::f32::consts::FRAC_PI_4),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Warp,
    composition_notes: "Angle is in radians. Defaults to π/4 (45°). Port-shadow on `angle` so a control wire (LFO / time) can rotate the mirror axis over time. UVs that fall outside [0,1] after the rotation round-trip are wrapped via `fract()` rather than clamped — sample with a Repeat sampler upstream if you want seamless behaviour.",
    examples: [],
    picker: { label: "Flip", category: Atom },
    summary: "Mirrors the image across a line through the centre at any angle, so one half becomes a reflection of the other. Set the angle for a horizontal, vertical, or diagonal flip.",
    category: DistortAndWarp,
    role: Filter,
    aliases: ["flip", "mirror axis", "mirror", "reflect", "Flip TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mirror_axis_body.wgsl"),
    input_access: [Gather],
}

impl Primitive for MirrorAxis {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle = match ctx.inputs.scalar("angle") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("angle") {
                Some(ParamValue::Float(f)) => *f,
                _ => std::f32::consts::FRAC_PI_4,
            },
        };

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
            // `in` is a Gather input (sampled at a body-computed mirrored UV).
            // Generated kernel binds uniform(0)/tex(1)/samp(2)/dst(3); the body
            // computes cos/sin from angle on the GPU (matching the hand).
            // mirror_axis.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.flip standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.flip",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MirrorAxisUniforms {
            angle,
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
            "node.flip",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::MirrorAxis;
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

    /// CPU reference of the WGSL math — mirror by `angle` around (0.5, 0.5).
    fn cpu_mirror(uv: (f32, f32), angle: f32) -> (f32, f32) {
        let (cx, cy) = (uv.0 - 0.5, uv.1 - 0.5);
        let (ca, sa) = (angle.cos(), angle.sin());
        let rx = cx * ca - cy * sa;
        let ry = cx * sa + cy * ca;
        let fy = ry.abs();
        let fx = rx;
        let ux = fx * ca + fy * sa;
        let uy = -fx * sa + fy * ca;
        ((ux + 0.5).fract(), (uy + 0.5).fract())
    }

    /// Identity test: a uniform source returns the same color regardless
    /// of mirror angle (every UV samples the same colour).
    #[test]
    fn uniform_input_unchanged() {
        let device = crate::test_device();
        let (w, h) = (8u32, 8u32);
        let format = GpuTextureFormat::Rgba16Float;
        let src_rgba = [0.4_f32, 0.6, 0.2, 1.0];

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(MirrorAxis::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "angle", ParamValue::Float(0.3)).unwrap();
        g.connect((src, "out"), (node, "in")).unwrap();
        g.connect((node, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "mirror-src");
        let out_target = RenderTarget::new(&device, w, h, format, "mirror-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [
                src_rgba[0] as f64,
                src_rgba[1] as f64,
                src_rgba[2] as f64,
                src_rgba[3] as f64,
            ],
            "mirror-src-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("mirror-frame");
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
        let mut readback_enc = device.create_encoder("mirror-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        for i in 0..(w * h) as usize {
            let r = f16::from_bits(pixels[i * 4]).to_f32();
            let gc = f16::from_bits(pixels[i * 4 + 1]).to_f32();
            let b = f16::from_bits(pixels[i * 4 + 2]).to_f32();
            assert!(
                (r - src_rgba[0]).abs() < 0.02
                    && (gc - src_rgba[1]).abs() < 0.02
                    && (b - src_rgba[2]).abs() < 0.02,
                "pixel {i}: ({r}, {gc}, {b}) vs src ({}, {}, {})",
                src_rgba[0],
                src_rgba[1],
                src_rgba[2]
            );
        }
    }

    #[test]
    fn cpu_reference_matches_legacy_metallic_glass() {
        // Spot-check the CPU mirror against the metallic_glass_process
        // legacy formulation: rotate -angle, fold |y|, rotate +angle,
        // fract(+0.5). Use a non-trivial angle and a few sample UVs.
        let angle = std::f32::consts::FRAC_PI_4;
        for (u, v) in [(0.25, 0.25), (0.75, 0.6), (0.1, 0.9), (0.5, 0.5)] {
            let (mu, mv) = cpu_mirror((u, v), angle);
            assert!(
                (0.0..=1.0).contains(&mu) && (0.0..=1.0).contains(&mv),
                "mirrored UV out of [0,1]: ({mu}, {mv}) from ({u}, {v})"
            );
        }
    }
}
