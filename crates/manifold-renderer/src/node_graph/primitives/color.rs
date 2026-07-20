//! Color-domain primitives: [`Brightness`], [`ChannelMix`], [`ColorRamp`].
//!
//! All three are pixel-local: each output pixel depends only on the same
//! input pixel and parameters. Converted onto the freeze codegen path
//! (2026-07-14, P3 wave 2) — `fusion_kind: Pointwise` + `wgsl_body`, so they
//! fuse with each other and with other pixel-local primitives. `ChannelMix`'s
//! four `Vec4` rows and `ColorRamp`'s two `Color` stops were the first
//! standalone-path users of those param types (`freeze/codegen.rs`'s
//! `ParamType::Vec4`/`ParamType::Color` branches, added this wave) — they
//! still fail `region.rs`'s scalar-only cut rule, so these three stay
//! individually-fusable (their own standalone dispatch) rather than folding
//! into a multi-node fused region; see each primitive's own doc note.

use std::borrow::Cow;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Public `TYPE_ID` re-exports for callers that pre-date the `primitive!`
/// macro conversion (2026-07-14, P3 wave 2) — `persistence.rs`'s registry
/// coverage test and `mod.rs`'s `pub use` both reference these by name.
/// `PrimitiveSpec::TYPE_ID` (`Brightness::TYPE_ID` etc.) carries the same
/// string; these constants are kept only for source compatibility.
pub const BRIGHTNESS_TYPE_ID: &str = "node.brightness";
pub const CHANNEL_MIX_TYPE_ID: &str = "node.channel_mixer";
pub const COLOR_RAMP_TYPE_ID: &str = "node.gradient_map";

// =====================================================================
// Brightness — RGB → grayscale via per-channel weights.
// =====================================================================

crate::primitive! {
    name: Brightness,
    type_id: "node.brightness",
    purpose: "RGB -> weighted grayscale (luma) via per-channel weights. Defaults are BT.709 luma coefficients, so the default behaviour is desaturate-to-luminance.",
    inputs: {
        source: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("weights"),
            label: "RGB Weights",
            ty: ParamType::Vec3,
            // Rec. 709 luma coefficients.
            default: ParamValue::Vec3([0.2126, 0.7152, 0.0722]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "The luma_for_height / luma_for_sobel pattern in MetallicGlass: collapse a colour field to a scalar before a heightmap or edge-detection pass.",
    examples: [],
    picker: { label: "Brightness", category: Atom },
    summary: "Collapses colour to a single brightness value using per-channel weights — the default weighting matches how the eye perceives luminance.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["brightness", "luma", "grayscale", "desaturate"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/brightness_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BrightnessUniforms {
    weights: [f32; 4], // xyz used; w padding
}

impl Primitive for Brightness {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let w = match ctx.params.get("weights") {
            Some(ParamValue::Vec3(v)) => *v,
            _ => [0.2126, 0.7152, 0.0722],
        };
        let uniforms = BrightnessUniforms {
            weights: [w[0], w[1], w[2], 0.0],
        };

        let Some(in_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. `shaders/
            // brightness.wgsl` is retained only as the gpu_tests parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.brightness standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.brightness",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.brightness",
        );
    }
}

// =====================================================================
// ChannelMix — 4x4 RGBA transformation.
// =====================================================================

crate::primitive! {
    name: ChannelMix,
    type_id: "node.channel_mixer",
    purpose: "Per-pixel 4x4 RGBA matrix transform: out = M . in, where M's rows are the four Vec4 params (row0=R, row1=G, row2=B, row3=A). Identity matrix is the param default — output = input.",
    inputs: {
        source: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("row0"),
            label: "Row 0 (R)",
            ty: ParamType::Vec4,
            default: ParamValue::Vec4([1.0, 0.0, 0.0, 0.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("row1"),
            label: "Row 1 (G)",
            ty: ParamType::Vec4,
            default: ParamValue::Vec4([0.0, 1.0, 0.0, 0.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("row2"),
            label: "Row 2 (B)",
            ty: ParamType::Vec4,
            default: ParamValue::Vec4([0.0, 0.0, 1.0, 0.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("row3"),
            label: "Row 3 (A)",
            ty: ParamType::Vec4,
            default: ParamValue::Vec4([0.0, 0.0, 0.0, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Common useful matrices — Swap A -> R: row0=(0,0,0,1), row1=(0,1,0,0), row2=(0,0,1,0), row3=(0,0,0,1). Luma drop: row0=row1=row2=(0.2126,0.7152,0.0722,0), row3=(0,0,0,1). Halation tint: row0=(1,0,0,0), row1=row2=(0,0,0,0). Isolate B: row0=row1=row2=(0,0,1,0), row3=(0,0,0,1).",
    examples: [],
    picker: { label: "Channel Mixer", category: Atom },
    summary: "Remaps RGBA channels through a 4x4 matrix — swap, isolate, or blend channels into each other.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["channel mixer", "channel mix", "swizzle", "matrix"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/channel_mix_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChannelMixUniforms {
    row0: [f32; 4],
    row1: [f32; 4],
    row2: [f32; 4],
    row3: [f32; 4],
}

impl Primitive for ChannelMix {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let row = |name: &str, default: [f32; 4]| -> [f32; 4] {
            match ctx.params.get(name) {
                Some(ParamValue::Vec4(v)) => *v,
                _ => default,
            }
        };
        let uniforms = ChannelMixUniforms {
            row0: row("row0", [1.0, 0.0, 0.0, 0.0]),
            row1: row("row1", [0.0, 1.0, 0.0, 0.0]),
            row2: row("row2", [0.0, 0.0, 1.0, 0.0]),
            row3: row("row3", [0.0, 0.0, 0.0, 1.0]),
        };

        let Some(in_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. `shaders/
            // channel_mix.wgsl` is retained only as the gpu_tests parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.channel_mixer standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.channel_mixer",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.channel_mixer",
        );
    }
}

// =====================================================================
// ColorRamp — luma → two-stop gradient lookup.
// =====================================================================

crate::primitive! {
    name: ColorRamp,
    type_id: "node.gradient_map",
    purpose: "Maps input luminance to a two-stop gradient (color_a at luma 0 -> color_b at luma 1). The gradient-map atom (Blender ColorRamp / TD Lookup with two stops). For richer multi-stop palettes (thermal, etc.) use node.lut1d with a supplied LUT texture.",
    inputs: {
        source: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color_a"),
            label: "Color A",
            ty: ParamType::Color,
            default: ParamValue::Color([0.0, 0.0, 0.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("color_b"),
            label: "Color B",
            ty: ParamType::Color,
            default: ParamValue::Color([1.0, 1.0, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Input is premultiplied alpha — unpremultiplied internally to read the true colour for the ramp index; a transparent input pixel stays transparent (keys over the layer below) rather than painting color_a as an opaque box.",
    examples: [],
    picker: { label: "Gradient Map", category: Atom },
    summary: "Recolours an image by mapping its brightness onto a two-colour gradient — dark areas become one colour, bright areas another.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["gradient map", "color ramp", "duotone", "lookup"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/color_ramp_body.wgsl"),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorRampUniforms {
    color_a: [f32; 4],
    color_b: [f32; 4],
}

impl Primitive for ColorRamp {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = |name: &str, default: [f32; 4]| -> [f32; 4] {
            match ctx.params.get(name) {
                Some(ParamValue::Color(c)) => *c,
                Some(ParamValue::Vec4(v)) => *v,
                _ => default,
            }
        };
        let uniforms = ColorRampUniforms {
            color_a: color("color_a", [0.0, 0.0, 0.0, 1.0]),
            color_b: color("color_b", [1.0, 1.0, 1.0, 1.0]),
        };

        let Some(in_tex) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path (mandatory for per-element GPU atoms): the kernel is
            // generated from `wgsl_body` so the atom fuses. `shaders/
            // color_ramp.wgsl` is retained only as the gpu_tests parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.gradient_map standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.gradient_map",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                manifold_gpu::GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                manifold_gpu::GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                manifold_gpu::GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.gradient_map",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod channel_mix_gpu_tests {
    //! Hardware tests for the channel_mix 4x4 matrix transform.
    //! Verify the canonical use cases: identity (default), A→R swizzle
    //! (the StarField use case), and channel isolation.
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::ChannelMix;
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

    /// Render a single ChannelMix node with the given matrix rows over
    /// an input texture cleared to `src_rgba`. Return the first pixel's
    /// RGBA as f32.
    fn run_channel_mix(
        src_rgba: [f32; 4],
        row0: [f32; 4],
        row1: [f32; 4],
        row2: [f32; 4],
        row3: [f32; 4],
    ) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(ChannelMix::new()));
        let sink = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "row0", ParamValue::Vec4(row0)).unwrap();
        g.set_param(node, "row1", ParamValue::Vec4(row1)).unwrap();
        g.set_param(node, "row2", ParamValue::Vec4(row2)).unwrap();
        g.set_param(node, "row3", ParamValue::Vec4(row3)).unwrap();
        g.connect((src, "out"), (node, "source")).unwrap();
        g.connect((node, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let r_out = output_resource(&plan, node, "out");
        let src_target = RenderTarget::new(&device, w, h, format, "channel-mix-src");
        let out_target = RenderTarget::new(&device, w, h, format, "channel-mix-out");
        crate::clear_texture_committed(
            &device,
            &src_target.texture,
            [
                src_rgba[0] as f64,
                src_rgba[1] as f64,
                src_rgba[2] as f64,
                src_rgba[3] as f64,
            ],
            "channel-mix-src-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("channel-mix-frame");
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
        let mut readback_enc = device.create_encoder("channel-mix-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(pixels[0]).to_f32(),
            f16::from_bits(pixels[1]).to_f32(),
            f16::from_bits(pixels[2]).to_f32(),
            f16::from_bits(pixels[3]).to_f32(),
        ]
    }

    /// Default matrix = identity. Output should match input.
    #[test]
    fn identity_matrix_preserves_input() {
        let src = [0.4_f32, 0.6, 0.2, 0.8];
        let out = run_channel_mix(
            src,
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        );
        for i in 0..4 {
            assert!(
                (out[i] - src[i]).abs() < 0.02,
                "identity matrix changed channel {i}: out={} src={}",
                out[i],
                src[i]
            );
        }
    }

    /// Swap A → R. With src.a = 0.8, expect R = 0.8 in output.
    /// (The StarField use case: voronoi cell_hash → R for downstream
    /// per-pixel math.)
    #[test]
    fn swap_a_to_r_moves_alpha_to_red() {
        let src = [0.4_f32, 0.6, 0.2, 0.8];
        let out = run_channel_mix(
            src,
            [0.0, 0.0, 0.0, 1.0], // R = src.a
            [0.0, 0.0, 0.0, 0.0], // G = 0
            [0.0, 0.0, 0.0, 0.0], // B = 0
            [0.0, 0.0, 0.0, 1.0], // A = src.a (passthrough)
        );
        assert!((out[0] - src[3]).abs() < 0.02, "R should equal src.a: out.r={}, src.a={}", out[0], src[3]);
        assert!(out[1].abs() < 0.02, "G should be zero: {}", out[1]);
        assert!(out[2].abs() < 0.02, "B should be zero: {}", out[2]);
        assert!((out[3] - src[3]).abs() < 0.02, "A should pass through: out.a={}, src.a={}", out[3], src[3]);
    }

    /// Luma drop: each output channel = Rec.709 luma of input RGB.
    #[test]
    fn luma_matrix_grayscales() {
        let src = [1.0_f32, 0.0, 0.0, 1.0]; // pure red
        let luma_row = [0.2126, 0.7152, 0.0722, 0.0];
        let out = run_channel_mix(
            src,
            luma_row,
            luma_row,
            luma_row,
            [0.0, 0.0, 0.0, 1.0],
        );
        let expected = 0.2126_f32;
        for (i, &val) in out.iter().enumerate().take(3) {
            assert!(
                (val - expected).abs() < 0.02,
                "luma channel {i}: out={val} expected={expected}",
            );
        }
        assert!((out[3] - 1.0).abs() < 0.02, "alpha passthrough: {}", out[3]);
    }
}

