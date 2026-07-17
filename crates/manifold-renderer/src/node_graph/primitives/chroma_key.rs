//! `node.chroma_key` — produce a mask from per-pixel colour proximity.
//!
//! Texture-to-mask primitive: examines each pixel's RGB and outputs a
//! grayscale value in `[0, 1]` describing how close that pixel is to
//! a target colour. The canonical "where is this colour" generator.
//!
//! Second Phase A primitive (per `docs/PRIMITIVE_LIBRARY_DESIGN.md`
//! §10). Pairs with `masked_mix` to make any effect operate selectively
//! on a chosen colour range — the immediate demo for both primitives
//! is the "Edge Stretch By Colour" preset, which stretches only the
//! pixels matching a user-picked target colour.
//!
//! Math is RGB Euclidean distance + smoothstep at the tolerance edge.
//! That's the simplest predictable model; HSV-distance (better for
//! hue selection irrespective of brightness) is a future variant.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Display labels for [`ChromaKey`]'s `mode` enum.
///   - `Select` (0, default): output 1 where the pixel matches the
///     key colour. Natural for "apply effect to this colour."
///   - `Reject` (1): output 0 at match. Traditional chroma-key /
///     greenscreen shape.
pub const CHROMA_KEY_MODES: &[&str] = &["Select", "Reject"];

crate::primitive! {
    name: ChromaKey,
    type_id: "node.chroma_key",
    purpose: "Produce a per-pixel mask describing how close each pixel is to a target colour (RGB Euclidean distance, soft falloff at the tolerance edge). Pairs with `masked_mix` to make any effect operate selectively on a chosen colour range.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("key_color"),
            label: "Key Colour",
            ty: ParamType::Vec3,
            default: ParamValue::Vec3([1.0, 0.0, 0.0]),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("tolerance"),
            label: "Tolerance",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            // Practical useful range. The RGB Euclidean distance can
            // technically reach √3 ≈ 1.732 (white↔black diagonal), but
            // any tolerance above 1.0 already selects ~all of typical
            // imagery; outer cards and the drift audit live in [0, 1].
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("softness"),
            label: "Softness",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: CHROMA_KEY_MODES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Output mask is written to all RGB channels so it's visible as grayscale in the editor; downstream `masked_mix` reads only `.r`. Tolerance is the RGB Euclidean distance threshold — values above ~0.5 already select most of typical imagery.",
    examples: [],
    picker: { label: "Chroma Key", category: Atom },
    summary: "Outputs a mask showing how close each pixel is to a chosen colour, the green-screen key. Feed it into a mask mix to knock out a background.",
    category: Mask,
    role: Filter,
    aliases: ["chroma key", "green screen", "keying", "Chroma Key TOP", "Color Key"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/chroma_key_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaKeyUniforms {
    key_r: f32,
    key_g: f32,
    key_b: f32,
    tolerance: f32,
    softness: f32,
    invert: u32,
    _pad0: f32,
    _pad1: f32,
}

impl Primitive for ChromaKey {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let key = match ctx.params.get("key_color") {
            Some(ParamValue::Vec3(v)) => *v,
            _ => [1.0, 0.0, 0.0],
        };
        let tolerance = match ctx.params.get("tolerance") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.3,
        };
        let softness = match ctx.params.get("softness") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.1,
        };
        let invert = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
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
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.chroma_key standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.chroma_key",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ChromaKeyUniforms {
            key_r: key[0],
            key_g: key[1],
            key_b: key[2],
            tolerance,
            softness,
            invert,
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
            "node.chroma_key",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! GPU correctness tests for `ChromaKey`.
    //!
    //! Test shape per §10.3:
    //!   1. Smoke — dispatches without panic.
    //!   2. Matching pixel → mask ~ 1 (Select mode, exact key colour).
    //!   3. Non-matching pixel → mask ~ 0 (very far from key).
    //!   4. Invert (Reject mode) flips both above.
    //!   5. Tolerance widening picks up further-out colours.

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        ParamValue, Source, compile, primitives::chroma_key::ChromaKey,
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

    /// Run `Source → ChromaKey → FinalOutput` with a solid-colour
    /// input. Returns the (0,0) output pixel.
    fn run_chroma_key_at(
        pixel: [f32; 4],
        key: [f32; 3],
        tolerance: f32,
        softness: f32,
        mode: u32,
    ) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let ck = g.add_node(Box::new(ChromaKey::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(ck, "key_color", ParamValue::Vec3(key)).unwrap();
        g.set_param(ck, "tolerance", ParamValue::Float(tolerance))
            .unwrap();
        g.set_param(ck, "softness", ParamValue::Float(softness))
            .unwrap();
        g.set_param(ck, "mode", ParamValue::Enum(mode)).unwrap();
        g.connect((src, "out"), (ck, "in")).unwrap();
        g.connect((ck, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_tgt = RenderTarget::new(&device, w, h, format, "chroma-key-src");
        let mut native_enc = device.create_encoder("chroma-key");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(
                &src_tgt.texture,
                pixel[0] as f64,
                pixel[1] as f64,
                pixel[2] as f64,
                pixel[3] as f64,
            );
        }

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_tgt);
        let ck_output_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(ck_output_slot)
            .expect("chroma_key output should be retained on backend");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let buf = device.create_buffer_shared(total_bytes);
        let mut rb_enc = device.create_encoder("chroma-key-readback");
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
    fn chroma_key_smoke_dispatch() {
        let out = run_chroma_key_at(
            [1.0, 0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            0.3,
            0.1,
            0, // Select
        );
        for c in out {
            assert!(c.is_finite(), "non-finite output channel {c}");
        }
    }

    #[test]
    fn chroma_key_select_matches_exact_key_color() {
        // Pixel == key color, Select mode, dist = 0 → mask should be 1.0.
        let out = run_chroma_key_at([1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.3, 0.1, 0);
        assert!(
            (out[0] - 1.0).abs() < 0.05,
            "mask.r should be ~1 at exact match, got {}",
            out[0]
        );
    }

    #[test]
    fn chroma_key_select_rejects_far_color() {
        // Pixel green, key red. dist = sqrt(2) ≈ 1.41, way outside
        // tolerance 0.3 + softness 0.1. Mask should be ~0.
        let out = run_chroma_key_at([0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.3, 0.1, 0);
        assert!(
            out[0] < 0.05,
            "mask.r should be ~0 at far color, got {}",
            out[0]
        );
    }

    #[test]
    fn chroma_key_reject_mode_inverts() {
        // Reject mode: exact match should produce mask ~0, far should
        // produce mask ~1. Inverse of Select.
        let match_out = run_chroma_key_at([1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.3, 0.1, 1);
        assert!(
            match_out[0] < 0.05,
            "Reject mode: mask.r should be ~0 at match, got {}",
            match_out[0]
        );
        let far_out = run_chroma_key_at([0.0, 1.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.3, 0.1, 1);
        assert!(
            (far_out[0] - 1.0).abs() < 0.05,
            "Reject mode: mask.r should be ~1 at far color, got {}",
            far_out[0]
        );
    }

    #[test]
    fn chroma_key_tolerance_widens_match_band() {
        // Pixel is medium red (0.5, 0.0, 0.0). Distance to key (1, 0, 0) = 0.5.
        // Tight tolerance 0.2 → outside, mask should be ~0.
        // Loose tolerance 0.7 → inside, mask should be ~1.
        let tight = run_chroma_key_at([0.5, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.2, 0.05, 0);
        let loose = run_chroma_key_at([0.5, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0], 0.7, 0.05, 0);
        assert!(
            tight[0] < 0.1,
            "tight tolerance: mask.r should be ~0 (dist=0.5 > tol=0.2), got {}",
            tight[0]
        );
        assert!(
            loose[0] > 0.9,
            "loose tolerance: mask.r should be ~1 (dist=0.5 < tol=0.7), got {}",
            loose[0]
        );
    }
}
