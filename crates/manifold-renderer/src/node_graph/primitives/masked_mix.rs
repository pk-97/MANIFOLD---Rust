//! `node.masked_mix` — three-texture blend with a per-pixel mask weight.
//!
//! The "where" partner to `mix`. `mix(a, b, amount)` blends two
//! textures with a single scalar weight; `masked_mix(a, b, mask, amount)`
//! takes a third texture whose red channel is read as a per-pixel
//! weight and modulated by `amount` globally.
//!
//! This is the first of the Phase A texture primitives (per
//! `docs/PRIMITIVE_LIBRARY_DESIGN.md` §10) — the foundational
//! mask-routing primitive that unlocks luma-keyed effects,
//! chroma-keyed effects, edge-gated stylize, threshold-bloom-in-shadows,
//! and every other "apply X only where Y" composition.
//!
//! The mask is sampled from `.r` by convention. Every mask-producing
//! primitive (`luma_key`, `chroma_key`, `threshold`) writes its scalar
//! result into the red channel, so a `Threshold → MaskedMix.mask`
//! wire is the canonical pattern. If a user wants to use a non-mask
//! texture's luminance instead, they wire it directly — the red
//! channel of an arbitrary RGB texture is just its red channel, not
//! its luma, which is sometimes useful (gating on a single colour
//! channel).

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: MaskedMix,
    type_id: "node.masked_mix",
    purpose: "Per-pixel blend of two textures, weighted by a third texture's red channel. The 'apply X only where Y' compositor: any threshold-style mask wired into `mask` selects where `b` overrides `a`. With no mask wired this behaves like `mix`.",
    inputs: {
        a: Texture2D required,
        b: Texture2D required,
        mask: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "Mask sampled from `.r`. Pair with `luma_key`, `chroma_key`, or `threshold` upstream of the mask input. The global `amount` scales the mask uniformly so the whole effect can be crossfaded in/out from one knob; at amount=0 the output is always `a`.",
    examples: ["preset.effect.glitch"],
    picker: { label: "Masked Mix", category: Atom },
    summary: "Blends two images using a third as a mask, applying one only where the mask is bright. The apply-only-where node.",
    category: Composite,
    role: Filter,
    aliases: ["masked mix", "mask blend", "composite"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/masked_mix_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskedMixUniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Primitive for MaskedMix {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(a) = ctx.inputs.texture_2d("a") else {
            return;
        };
        let Some(b) = ctx.inputs.texture_2d("b") else {
            return;
        };
        let Some(mask) = ctx.inputs.texture_2d("mask") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.masked_mix standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.masked_mix",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MaskedMixUniforms {
            amount,
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
                    texture: a,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: b,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: mask,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 5,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.masked_mix",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! GPU correctness tests for `MaskedMix`.
    //!
    //! Test shape (per §10.3 — no legacy baseline exists for new
    //! Phase A primitives):
    //!   1. Smoke — dispatches without panic, pipeline compiles.
    //!   2. Identity — mask = 0 returns A unchanged.
    //!   3. Full     — mask = 1 returns B.
    //!   4. Amount zero — amount = 0 returns A regardless of mask.

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        ParamValue, Source, compile, primitives::masked_mix::MaskedMix,
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

    /// Run `Source × 3 → MaskedMix → FinalOutput` on 4×4 solid-colour
    /// inputs and return the (0,0) pixel. All pixels are identical for
    /// solid-colour inputs.
    fn run_masked_mix_at(
        a_rgba: [f32; 4],
        b_rgba: [f32; 4],
        mask_rgba: [f32; 4],
        amount: f32,
    ) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src_a = g.add_node(Box::new(Source::new()));
        let src_b = g.add_node(Box::new(Source::new()));
        let src_m = g.add_node(Box::new(Source::new()));
        let mm = g.add_node(Box::new(MaskedMix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mm, "amount", ParamValue::Float(amount)).unwrap();
        g.connect((src_a, "out"), (mm, "a")).unwrap();
        g.connect((src_b, "out"), (mm, "b")).unwrap();
        g.connect((src_m, "out"), (mm, "mask")).unwrap();
        g.connect((mm, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_a = output_resource(&plan, src_a, "out");
        let r_b = output_resource(&plan, src_b, "out");
        let r_m = output_resource(&plan, src_m, "out");
        let a_tgt = RenderTarget::new(&device, w, h, format, "masked-mix-a");
        let b_tgt = RenderTarget::new(&device, w, h, format, "masked-mix-b");
        let m_tgt = RenderTarget::new(&device, w, h, format, "masked-mix-mask");
        let mut native_enc = device.create_encoder("masked-mix");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(
                &a_tgt.texture,
                a_rgba[0] as f64,
                a_rgba[1] as f64,
                a_rgba[2] as f64,
                a_rgba[3] as f64,
            );
            gpu.clear_texture(
                &b_tgt.texture,
                b_rgba[0] as f64,
                b_rgba[1] as f64,
                b_rgba[2] as f64,
                b_rgba[3] as f64,
            );
            gpu.clear_texture(
                &m_tgt.texture,
                mask_rgba[0] as f64,
                mask_rgba[1] as f64,
                mask_rgba[2] as f64,
                mask_rgba[3] as f64,
            );
        }

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_a, a_tgt);
        backend.pre_bind_texture_2d(r_b, b_tgt);
        backend.pre_bind_texture_2d(r_m, m_tgt);
        let mm_output_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(mm_output_slot)
            .expect("masked_mix output should be retained on backend");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let buf = device.create_buffer_shared(total_bytes);
        let mut rb_enc = device.create_encoder("masked-mix-readback");
        rb_enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
        rb_enc.commit_and_wait_completed();

        let ptr = buf
            .mapped_ptr()
            .expect("shared buffer should expose mapped pointer");
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
    fn masked_mix_smoke_dispatch() {
        let out = run_masked_mix_at(
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            1.0,
        );
        // Just check we got real numbers, not NaN/Inf.
        for c in out {
            assert!(c.is_finite(), "non-finite output channel {c}");
        }
    }

    #[test]
    fn masked_mix_zero_mask_returns_a() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.3, 0.5, 0.8, 1.0];
        let mask_off = [0.0, 0.0, 0.0, 1.0];
        let out = run_masked_mix_at(a, b, mask_off, 1.0);
        let tol = 0.01;
        for c in 0..4 {
            assert!(
                (out[c] - a[c]).abs() < tol,
                "channel {c}: got {} expected a={} with zero mask",
                out[c],
                a[c]
            );
        }
    }

    #[test]
    fn masked_mix_full_mask_returns_b() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.3, 0.5, 0.8, 1.0];
        let mask_on = [1.0, 0.0, 0.0, 1.0]; // only .r is read
        let out = run_masked_mix_at(a, b, mask_on, 1.0);
        let tol = 0.01;
        for c in 0..4 {
            assert!(
                (out[c] - b[c]).abs() < tol,
                "channel {c}: got {} expected b={} with full mask",
                out[c],
                b[c]
            );
        }
    }

    #[test]
    fn masked_mix_amount_zero_returns_a_regardless_of_mask() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.3, 0.5, 0.8, 1.0];
        let mask_on = [1.0, 0.0, 0.0, 1.0];
        let out = run_masked_mix_at(a, b, mask_on, 0.0);
        let tol = 0.01;
        for c in 0..4 {
            assert!(
                (out[c] - a[c]).abs() < tol,
                "channel {c}: got {} expected a={} with amount=0",
                out[c],
                a[c]
            );
        }
    }

    #[test]
    fn masked_mix_half_mask_lerps() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.8, 0.0, 1.0, 1.0];
        let mask_half = [0.5, 0.0, 0.0, 1.0];
        let out = run_masked_mix_at(a, b, mask_half, 1.0);
        let tol = 0.01;
        let expected = [
            (a[0] + b[0]) * 0.5,
            (a[1] + b[1]) * 0.5,
            (a[2] + b[2]) * 0.5,
            (a[3] + b[3]) * 0.5,
        ];
        for c in 0..4 {
            assert!(
                (out[c] - expected[c]).abs() < tol,
                "channel {c}: got {} expected {} with half mask",
                out[c],
                expected[c]
            );
        }
    }
}
