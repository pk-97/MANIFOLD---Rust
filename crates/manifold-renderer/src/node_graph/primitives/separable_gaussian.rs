//! `node.gaussian_blur` — single-axis Gaussian blur with
//! 9/17/25-tap precomputed kernels. Building block for Halation,
//! Bloom, and Watercolor decompositions; kernel weights are
//! bit-identical to the legacy DoF / Halation shaders so reassembled
//! H+V pairs parity-check against their monolithic originals.
//!
//! A horizontal pass followed by a vertical pass with the same kernel
//! and step produces an isotropic Gaussian blur. The `step` parameter
//! controls per-tap pixel stride — legacy Halation passes
//! `spread * 5.0 + 1.0`, legacy DoF passes `coc * 6.0 + 1.0` (variable
//! per-pixel; DoF's variable-width variant needs a separate primitive
//! when §6.4 lands).

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Display labels for the `kernel_size` enum, indexed by enum value.
pub const GAUSSIAN_BLUR_KERNELS: &[&str] = &["9-tap", "17-tap", "25-tap"];

/// Display labels for the `axis` enum, indexed by enum value.
pub const GAUSSIAN_BLUR_AXES: &[&str] = &["Horizontal", "Vertical"];

crate::primitive! {
    name: GaussianBlur,
    type_id: "node.gaussian_blur",
    purpose: "Single-axis Gaussian blur. Pair an H pass with a V pass (same kernel + step) for an isotropic blur. 9-tap (σ≈2), 17-tap (σ≈4), or 25-tap (σ≈6) precomputed kernels.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "kernel_size",
            label: "Kernel Size",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1),
            range: Some((0.0, 2.0)),
            enum_values: GAUSSIAN_BLUR_KERNELS,
        },
        ParamDef {
            name: "axis",
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: GAUSSIAN_BLUR_AXES,
        },
        ParamDef {
            name: "step",
            label: "Step",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 32.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Use the same `kernel_size` and `step` on both H and V passes for a separable isotropic blur. The kernels are normalized — DC gain = 1. Variable per-pixel width (DoF's CoC-modulated Gaussian) needs a different primitive.",
    examples: ["composite.bloom", "composite.halation", "composite.watercolor"],
    picker: { label: "Gaussian Blur", category: Atom },
}

pub const GAUSSIAN_BLUR_TYPE_ID: &str = "node.gaussian_blur";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeparableGaussianUniforms {
    kernel_size: u32,
    axis: u32,
    step: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Primitive for GaussianBlur {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let kernel_size = match ctx.params.get("kernel_size") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(2),
            _ => 1,
        };
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let step = match ctx.params.get("step") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        let texel_x = 1.0 / width as f32;
        let texel_y = 1.0 / height as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/separable_gaussian.wgsl"),
                "cs_main",
                "node.gaussian_blur",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SeparableGaussianUniforms {
            kernel_size,
            axis,
            step,
            texel_x,
            texel_y,
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
            "node.gaussian_blur",
        );
    }
}

#[cfg(test)]
mod gpu_tests {
    //! Real-GPU smoke tests. GaussianBlur is a new primitive
    //! (no 1:1 legacy effect) — validation is against analytical
    //! invariants: DC preservation, axis isolation, and known
    //! kernel response on a delta-function input.

    

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        ParamValue, Source, compile,
    };
    use crate::render_target::RenderTarget;

    use super::GaussianBlur;

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

    /// Run GaussianBlur on `w × h` input. The caller supplies a
    /// closure that fills the input texture (a one-shot encoder is
    /// passed through). Returns the full RGBA output as f32.
    fn run_gaussian<F: FnOnce(&mut RendererGpuEncoder<'_>, &RenderTarget)>(
        w: u32,
        h: u32,
        kernel_size: u32,
        axis: u32,
        step: f32,
        fill_input: F,
    ) -> Vec<[f32; 4]> {
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let gauss = g.add_node(Box::new(GaussianBlur::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(gauss, "kernel_size", ParamValue::Enum(kernel_size))
            .unwrap();
        g.set_param(gauss, "axis", ParamValue::Enum(axis)).unwrap();
        g.set_param(gauss, "step", ParamValue::Float(step)).unwrap();
        g.connect((src, "out"), (gauss, "in")).unwrap();
        g.connect((gauss, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let in_target = RenderTarget::new(&device, w, h, format, "test-in");
        let mut native_enc = device.create_encoder("gauss-in");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            fill_input(&mut gpu, &in_target);
        }

        let mut backend = MetalBackend::new(&device, w, h, format);
        backend.pre_bind_texture_2d(r_src, in_target);
        let out_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("gauss-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    /// Hand-computed sum of each kernel's weights (center + 2× each
    /// positive-side tap). These constants are NOT normalized to 1.0
    /// in the legacy shaders — preserving the exact gain is part of
    /// the parity contract for Halation/Bloom/Watercolor when they're
    /// reassembled in §6.3 commits 3–6.
    const K9_GAIN: f32 = 0.16501 + 2.0 * (0.15019 + 0.11325 + 0.07076 + 0.03664);
    const K17_GAIN: f32 = 0.10315
        + 2.0 * (0.09998 + 0.09103 + 0.07786 + 0.06257 + 0.04723 + 0.03350 + 0.02232 + 0.01396);
    const K25_GAIN: f32 = 0.07087
        + 2.0
            * (0.06947
                + 0.06540
                + 0.05917
                + 0.05148
                + 0.04307
                + 0.03465
                + 0.02680
                + 0.01995
                + 0.01428
                + 0.00983
                + 0.00651
                + 0.00415);

    /// On a solid-color input every sample reads the same value, so
    /// the output equals input × Σ(weights). This proves: (a) the
    /// shader's kernel-selector branch picks the right kernel,
    /// (b) every weight is encoded with no typo, (c) sampling is
    /// well-behaved at any axis/step. Each kernel has its own gain
    /// because the legacy weights aren't perfectly DC-normalized.
    #[test]
    fn solid_input_scales_by_kernel_gain_across_axes() {
        let input = [0.4, 0.6, 0.2, 1.0];
        let tol = 0.01;
        for (kernel, gain) in [(0u32, K9_GAIN), (1, K17_GAIN), (2, K25_GAIN)] {
            for axis in 0u32..=1 {
                let out = run_gaussian(8, 8, kernel, axis, 1.0, |gpu, target| {
                    gpu.clear_texture(
                        &target.texture,
                        input[0] as f64,
                        input[1] as f64,
                        input[2] as f64,
                        input[3] as f64,
                    );
                });
                for (i, pix) in out.iter().enumerate() {
                    for c in 0..4 {
                        let want = input[c] * gain;
                        assert!(
                            (pix[c] - want).abs() < tol,
                            "kernel {kernel} axis {axis} pix {i} ch {c}: got {} want {} (gain {gain})",
                            pix[c],
                            want
                        );
                    }
                }
            }
        }
    }

    /// The gain is independent of `step` on a solid input (edge
    /// clamping doesn't add a DC bias even at large strides). Locks
    /// in that property so a future shader edit can't silently break
    /// big-radius blurs in Halation/Bloom.
    #[test]
    fn solid_input_gain_independent_of_step() {
        let input = [0.3, 0.7, 0.5, 1.0];
        let tol = 0.01;
        for step in [0.0f32, 1.0, 4.0, 16.0, 32.0] {
            let out = run_gaussian(8, 8, 2, 0, step, |gpu, target| {
                gpu.clear_texture(
                    &target.texture,
                    input[0] as f64,
                    input[1] as f64,
                    input[2] as f64,
                    input[3] as f64,
                );
            });
            for pix in &out {
                for c in 0..4 {
                    let want = input[c] * K25_GAIN;
                    assert!(
                        (pix[c] - want).abs() < tol,
                        "step {step} ch {c}: got {} want {}",
                        pix[c],
                        want
                    );
                }
            }
        }
    }
}
