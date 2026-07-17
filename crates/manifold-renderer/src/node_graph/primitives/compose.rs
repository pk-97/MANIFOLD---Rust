//! Composition primitives — combine two textures into one.
//!
//! [`Mix`] is the unified compositing primitive: blend `b` on top of `a`
//! using one of 7 modes, then crossfade the result back against `a` by
//! `amount`. At `mode = Lerp` it's a pure linear crossfade; at any
//! other mode `amount` acts as opacity for the blend result. Pixel-local
//! and fuseable. It supersedes the old no-op `Blend` stub (now removed).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// =====================================================================
// Mix — combine A and B with a blend mode, crossfaded by amount.
// =====================================================================

/// Display labels for [`Mix`]'s `mode` enum. Index = enum value:
/// 0=Lerp, 1=Screen, 2=Add, 3=Max, 4=Multiply, 5=Difference, 6=Overlay, 7=Divide.
///
/// `Lerp` collapses the crossfade to pure linear interpolation —
/// `out = mix(a, b, amount)` — and is the default. Every other mode
/// computes `blend(a,b)` then mixes it back over `a` by `amount`, so
/// `amount = 0` always returns `a` unchanged regardless of mode.
pub const MIX_MODES: &[&str] = &[
    "Lerp",
    "Screen",
    "Add",
    "Max",
    "Multiply",
    "Difference",
    "Overlay",
    "Divide",
];

crate::primitive! {
    name: Mix,
    type_id: "node.mix",
    purpose: "Combine two textures with one of 8 blend modes (Lerp, Screen, Add, Max, Multiply, Difference, Overlay, Divide), crossfaded back against A by `amount`. At amount=0 returns A unchanged; at amount=1 returns the full blended result. Lerp mode is a pure linear crossfade. Divide mode is per-channel `a / b`, guarded against divide-by-near-zero (returns 0 when `b` is below epsilon — same convention as `node.array_math` Divide).",
    inputs: {
        a: Texture2D required,
        b: Texture2D required,
        amount: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Blend Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 7.0)),
            enum_values: MIX_MODES,
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "Use Lerp for pure crossfades; Add/Screen for additive bloom-style merges; Multiply for darkening masks; Max for tonemap-safe brightening; Overlay for contrast-preserving combines; Divide for per-channel `a/b` (useful for normalising one field by another — e.g. density-driven scaling fields in fluid sims). Divide guards against divide-by-near-zero by returning 0 when `b` is below 1e-6.",
    examples: ["composite.bloom", "composite.halation"],
    picker: { label: "Mix", category: Atom },
    summary: "Blends two images together with a choice of modes like Add, Screen, Multiply, and Overlay, plus a crossfade amount. The core layer-blend node.",
    category: Composite,
    role: Filter,
    aliases: ["mix", "blend", "composite", "Composite TOP", "Mix Color"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/mix_body.wgsl"),
}

pub const MIX_TYPE_ID: &str = "node.mix";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixUniforms {
    amount: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
}

impl Primitive for Mix {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // `amount` port-shadows the param — a wired scalar (LFO, gate,
        // envelope, a shared master knob via node.value) drives the
        // crossfade live; the inline param is the fallback.
        let amount = match ctx.inputs.scalar("amount") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("amount") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.5,
            },
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(7),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(7),
            _ => 0,
        };

        let Some(a) = ctx.inputs.texture_2d("a") else {
            return;
        };
        let Some(b) = ctx.inputs.texture_2d("b") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out.width, out.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.mix standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mix",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MixUniforms {
            amount,
            mode,
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
                    texture: a,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: b,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.mix",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU integration tests. These spin up a `manifold_gpu::GpuDevice`,
    //! a `MetalBackend`, and an actual `GpuEncoder`, then run the graph
    //! end-to-end. Mac-only (Metal).
    //!
    //! Goal: catch wiring bugs (binding indices, format mismatches,
    //! pipeline compilation failures, missing usages) and prove pixel
    //! correctness — bugs that mock-backend tests can't see.

    

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        ParamValue, Source, compile, primitives::compose::Mix,
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

    /// End-to-end smoke test: build `Source × 2 → Mix → FinalOutput`,
    /// dispatch through `Executor::execute_frame_with_gpu`, commit, wait.
    /// Verifies the whole stack — pipeline compile, binding layout,
    /// MetalBackend slot allocation, encoder dispatch — works on a real
    /// `GpuDevice`. No pixel check; that's `mix_pixel_correct_at_half`.
    #[test]
    fn mix_dispatches_through_metal_backend() {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        // Build graph.
        let mut g = Graph::new();
        let src_a = g.add_node(Box::new(Source::new()));
        let src_b = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mix, "amount", ParamValue::Float(0.5)).unwrap();
        g.connect((src_a, "out"), (mix, "a")).unwrap();
        g.connect((src_b, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        // Wire backend + encoder.
        let backend = MetalBackend::new(device.arc(), w, h, format);
        let mut native_enc = device.create_encoder("mix-smoke");
        let mut exec = Executor::new(Box::new(backend));

        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }

        // Synchronously commit + wait so any Metal validation error or
        // shader compile failure surfaces inside the test instead of
        // dangling on the GPU.
        native_enc.commit_and_wait_completed();
    }

    /// Pixel-accurate proof of correctness. Pre-binds host-supplied
    /// red and blue input textures to the two `Source` nodes via
    /// `MetalBackend::pre_bind_texture_2d`, runs Mix at amount=0.5, and
    /// reads back Mix's output. Expected per-pixel: (0.5, 0.0, 0.5, 1.0)
    /// (within f16 precision tolerance). This is the proof that
    /// shader bindings, slot allocation, idempotent acquire, and the
    /// dispatch math are all wired correctly.
    #[test]
    fn mix_pixel_correct_at_half() {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        // Build graph.
        let mut g = Graph::new();
        let src_a = g.add_node(Box::new(Source::new()));
        let src_b = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mix, "amount", ParamValue::Float(0.5)).unwrap();
        g.connect((src_a, "out"), (mix, "a")).unwrap();
        g.connect((src_b, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        // Look up Source A/B's output ResourceIds — what Mix.a and Mix.b
        // will read from after pre-binding.
        let r_a = output_resource(&plan, src_a, "out");
        let r_b = output_resource(&plan, src_b, "out");

        // Allocate the input textures and clear them with known colors.
        let red_target = RenderTarget::new(&device, w, h, format, "test-red");
        let blue_target = RenderTarget::new(&device, w, h, format, "test-blue");
        let mut native_enc = device.create_encoder("mix-pixel");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&red_target.texture, 1.0, 0.0, 0.0, 1.0);
            gpu.clear_texture(&blue_target.texture, 0.0, 0.0, 1.0, 1.0);
        }

        // Pre-bind the colored targets to the Source output ResourceIds.
        // Capture the next-slot watermark — Mix's output will be allocated
        // there since the Texture2D free pool is empty post-pre-bind.
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_a, red_target);
        backend.pre_bind_texture_2d(r_b, blue_target);
        let mix_output_slot = Slot(backend.slot_count());

        // Execute the dispatch in the same encoder as the input clears.
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        // Blit Mix's output texture into a CPU-mapped buffer for readback.
        let mix_tex = exec
            .backend()
            .texture_2d(mix_output_slot)
            .expect("mix output texture should be retained on backend");
        let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/pixel.
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("mix-readback");
        readback_enc.copy_texture_to_buffer(mix_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        // Verify pixel (0,0). Solid colors mean every pixel matches.
        let ptr = readback_buf
            .mapped_ptr()
            .expect("shared buffer should expose mapped pointer");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let r = f16::from_bits(pixels[0]).to_f32();
        let g_chan = f16::from_bits(pixels[1]).to_f32();
        let b = f16::from_bits(pixels[2]).to_f32();
        let a = f16::from_bits(pixels[3]).to_f32();
        let tol = 0.01;
        assert!(
            (r - 0.5).abs() < tol,
            "red channel {r} != 0.5 (mix(1,0,0.5))"
        );
        assert!(g_chan.abs() < tol, "green {g_chan} != 0.0");
        assert!((b - 0.5).abs() < tol, "blue {b} != 0.5 (mix(0,1,0.5))");
        assert!((a - 1.0).abs() < tol, "alpha {a} != 1.0");
    }

    /// Run Mix end-to-end on 4×4 solid-color inputs and return the
    /// (0,0) pixel (every pixel is identical for solid-color inputs).
    /// Shared by the per-mode smoke tests below.
    fn run_mix_at(a_rgba: [f32; 4], b_rgba: [f32; 4], mode: u32, amount: f32) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src_a = g.add_node(Box::new(Source::new()));
        let src_b = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mix, "amount", ParamValue::Float(amount))
            .unwrap();
        g.set_param(mix, "mode", ParamValue::Enum(mode)).unwrap();
        g.connect((src_a, "out"), (mix, "a")).unwrap();
        g.connect((src_b, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_a = output_resource(&plan, src_a, "out");
        let r_b = output_resource(&plan, src_b, "out");
        let a_target = RenderTarget::new(&device, w, h, format, "test-a");
        let b_target = RenderTarget::new(&device, w, h, format, "test-b");
        let mut native_enc = device.create_encoder("mix-modes");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(
                &a_target.texture,
                a_rgba[0] as f64,
                a_rgba[1] as f64,
                a_rgba[2] as f64,
                a_rgba[3] as f64,
            );
            gpu.clear_texture(
                &b_target.texture,
                b_rgba[0] as f64,
                b_rgba[1] as f64,
                b_rgba[2] as f64,
                b_rgba[3] as f64,
            );
        }

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_a, a_target);
        backend.pre_bind_texture_2d(r_b, b_target);
        let mix_output_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let mix_tex = exec
            .backend()
            .texture_2d(mix_output_slot)
            .expect("mix output texture should be retained on backend");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("mix-modes-readback");
        readback_enc.copy_texture_to_buffer(mix_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf
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

    /// At amount = 0 the output must always be A, regardless of mode.
    #[test]
    fn mix_amount_zero_returns_a_for_all_modes() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.3, 0.5, 0.8, 1.0];
        let tol = 0.01;
        for mode in 0u32..=7 {
            let out = run_mix_at(a, b, mode, 0.0);
            for c in 0..4 {
                assert!(
                    (out[c] - a[c]).abs() < tol,
                    "mode {mode} channel {c}: {} != a={} (amount=0)",
                    out[c],
                    a[c]
                );
            }
        }
    }

    /// Divide mode guards against divide-by-near-zero — when `b` is below
    /// the 1e-6 epsilon, the channel returns 0 rather than producing NaN/Inf.
    #[test]
    fn mix_divide_guards_against_near_zero_b() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.0, 0.0, 0.0, 1.0];
        let tol = 0.01;
        let out = run_mix_at(a, b, 7, 1.0);
        for (c, &val) in out.iter().enumerate().take(3) {
            assert!(
                val.abs() < tol,
                "Divide channel {c}: got {val} expected 0.0 (b near zero)",
            );
        }
    }

    /// At amount = 1, each mode computes the pure blend of A and B.
    /// The expected values below are hand-derived from the per-mode
    /// formulas documented in `shaders/mix.wgsl`.
    #[test]
    fn mix_modes_apply_correct_blend_at_amount_one() {
        let a = [0.4, 0.6, 0.2, 1.0];
        let b = [0.3, 0.5, 0.8, 1.0];
        let tol = 0.01;
        let expected: [(u32, &str, [f32; 3]); 8] = [
            (0, "Lerp", [0.3, 0.5, 0.8]),
            (1, "Screen", [0.58, 0.8, 0.84]),
            (2, "Add", [0.7, 1.1, 1.0]),
            (3, "Max", [0.4, 0.6, 0.8]),
            (4, "Multiply", [0.12, 0.3, 0.16]),
            (5, "Difference", [0.1, 0.1, 0.6]),
            (6, "Overlay", [0.24, 0.6, 0.32]),
            // Divide: a / b per-channel — 0.4/0.3=1.333, 0.6/0.5=1.2, 0.2/0.8=0.25
            (7, "Divide", [1.333, 1.2, 0.25]),
        ];
        for (mode, label, want_rgb) in expected {
            let out = run_mix_at(a, b, mode, 1.0);
            for c in 0..3 {
                assert!(
                    (out[c] - want_rgb[c]).abs() < tol,
                    "{label} (mode {mode}) channel {c}: got {} expected {}",
                    out[c],
                    want_rgb[c]
                );
            }
        }
    }

    /// BUG-181: non-Lerp blend modes are RGB-only and pass `a`'s alpha
    /// through untouched, regardless of `amount` — a data texture's filler
    /// alpha (e.g. an SSAO map's alpha=1) must not overwrite a display
    /// chain's real alpha. Lerp (mode 0) is the one genuine crossfade and
    /// still lerps alpha a->b.
    #[test]
    fn mix_alpha_passes_through_a_in_non_lerp_modes_but_lerps_in_lerp_mode() {
        let a = [0.4, 0.6, 0.2, 0.25];
        let b = [0.3, 0.5, 0.8, 1.0];
        let tol = 0.01;

        // Multiply, amount=1.0: alpha must be a.a=0.25 regardless of amount.
        let out_multiply = run_mix_at(a, b, 4, 1.0);
        assert!(
            (out_multiply[3] - 0.25).abs() < tol,
            "Multiply alpha {} != 0.25 (a.a pass-through)",
            out_multiply[3]
        );

        // Lerp, amount=0.5: alpha crossfades a->b: 0.25*0.5 + 1.0*0.5 = 0.625.
        let out_lerp = run_mix_at(a, b, 0, 0.5);
        assert!(
            (out_lerp[3] - 0.625).abs() < tol,
            "Lerp alpha {} != 0.625 (mix(0.25, 1.0, 0.5))",
            out_lerp[3]
        );
    }
}
