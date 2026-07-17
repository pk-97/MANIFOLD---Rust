//! `node.reinhard_tone_map` — tone mapping on an HDR Texture2D, in one
//! of three curves selected by the `curve` enum:
//!
//! - **Extended** (default): per-channel `x * (1 + x/9) / (1 + x)`,
//!   matches the FluidSim display path bit-for-bit. Preserves more
//!   high values than Simple — visible difference on bright highlights
//!   (specular peaks).
//! - **Simple**: per-channel `x / (x + 1)`, the textbook Reinhard
//!   curve. Crushes highlights more aggressively. Matches the legacy
//!   MetallicGlass render terminal bit-for-bit.
//! - **Log**: per-channel `log2(1 + x) / log2(1 + 64)` — the
//!   flame-fractal response for particle-density pipelines; reveals
//!   structure across the faint-to-hot range that Reinhard compresses
//!   away. White point fixed at 64.0 (constant-in-primitive, like
//!   Extended's fixed 3.0); `intensity` is the exposure ride.
//!
//! SDR-only. For multi-curve / HDR-aware tone mapping (ACES, AgX,
//! Khronos PBR, PQ / EDR output), use `node.tone_map` instead.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ReinhardUniforms {
    intensity: f32,
    contrast: f32,
    curve: u32, // 0 = Extended (default), 1 = Simple (x/(x+1))
    _pad0: f32,
}

crate::primitive! {
    name: ReinhardToneMap,
    type_id: "node.reinhard_tone_map",
    purpose: "Tone mapping for HDR display in one of three curves: Extended (default — `x*(1+x/9)/(1+x)`, matches FluidSim bit-for-bit, preserves highlights), Simple (`x/(x+1)`, the textbook Reinhard curve, matches the legacy MetallicGlass render terminal bit-for-bit), or Log (`log2(1+x)/log2(1+64)` — the flame-fractal response for particle-density pipelines: reveals faint accumulation structure Reinhard compresses away; ride intensity as the exposure). intensity + contrast are port-shadowed pre-multipliers — wire a `node.canvas_area_scale → node.math` chain into `intensity` for resolution-aware brightness compensation in particle-density pipelines. SDR-only — for HDR-aware (PQ / EDR) or alternate curves (ACES / AgX / Khronos PBR Neutral), use `node.tone_map`.",
    inputs: {
        in: Texture2D required,
        intensity: ScalarF32 optional,
        contrast: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("contrast"),
            label: "Contrast",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 8.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("curve"),
            label: "Curve",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: &["Extended", "Simple", "Log"],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "intensity scales the pre-tonemap signal; contrast is a second multiplier. Both port-shadowed for runtime modulation (canvas-area brightness comp, audio-driven dynamics). Extended white-point fixed at 3.0 (FluidSim default). Simple curve is bit-exact `x/(x+1)` — pick this when matching a legacy renderer that used textbook Reinhard. Log (white fixed at 64.0) is the default grade for accumulated-density renders (resolve_scatter/resolve_accumulator output) — faint single-particle deposits stay visible against multi-thousand-hit hot spots. Output alpha = source alpha. For HDR pipelines that need parameterised white-point or alternate curves, swap in `node.tone_map`.",
    examples: [],
    picker: { label: "Reinhard Tone Map", category: Atom },
    summary: "A simpler HDR-to-display tone map using the Reinhard curve. Lighter weight than the full Tone Map node.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["reinhard", "tonemap", "hdr"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/reinhard_tone_map_body.wgsl"),
}

impl Primitive for ReinhardToneMap {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let intensity = ctx.scalar_or_param("intensity", 1.0);
        let contrast = ctx.scalar_or_param("contrast", 1.0);
        let curve: u32 = match ctx.params.get("curve") {
            Some(ParamValue::Enum(v)) => *v,
            _ => 0,
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let width = target.width;
        let height = target.height;
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.reinhard_tone_map standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.reinhard_tone_map",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ReinhardUniforms {
            intensity,
            contrast,
            curve,
            _pad0: 0.0,
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
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.reinhard_tone_map",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn reinhard_declares_texture_in_and_out_plus_port_shadowed_scalars() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(ReinhardToneMap::TYPE_ID, "node.reinhard_tone_map");
        let in_port = ReinhardToneMap::INPUTS
            .iter()
            .find(|p| p.name == "in")
            .unwrap();
        assert_eq!(in_port.ty, PortType::Texture2D);
        assert!(in_port.required);

        // Port-shadows-param: intensity + contrast as optional scalar
        // inputs so a math chain (canvas_area_scale, audio-driven) can
        // drive them at runtime.
        for name in ["intensity", "contrast"] {
            let port = ReinhardToneMap::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("missing port-shadow input `{name}`"));
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
            assert!(!port.required);
        }

        assert_eq!(ReinhardToneMap::OUTPUTS.len(), 1);
        assert_eq!(ReinhardToneMap::OUTPUTS[0].name, "out");
        assert_eq!(ReinhardToneMap::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn reinhard_has_intensity_and_contrast_params() {
        let names: Vec<&str> = ReinhardToneMap::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(names, vec!["intensity", "contrast", "curve"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = ReinhardToneMap::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.reinhard_tone_map");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Value-level GPU tests for the three tone curves. Extended and
    //! Simple rows are regression pins (bit-behaviour must survive the
    //! Log addition); Log rows verify the new arm at hand-computed
    //! (x, expected) pairs.

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::ReinhardToneMap;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        ParamValue, Source, compile,
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

    /// Run `Source → ReinhardToneMap → FinalOutput` on a solid-colour
    /// input and return the (0,0) output pixel.
    fn run_tone_map_at(pixel: f32, intensity: f32, contrast: f32, curve: u32) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let tm = g.add_node(Box::new(ReinhardToneMap::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(tm, "intensity", ParamValue::Float(intensity)).unwrap();
        g.set_param(tm, "contrast", ParamValue::Float(contrast)).unwrap();
        g.set_param(tm, "curve", ParamValue::Enum(curve)).unwrap();
        g.connect((src, "out"), (tm, "in")).unwrap();
        g.connect((tm, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src = output_resource(&plan, src, "out");
        let src_tgt = RenderTarget::new(&device, w, h, format, "reinhard-src");
        let mut native_enc = device.create_encoder("reinhard-test");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&src_tgt.texture, f64::from(pixel), f64::from(pixel), f64::from(pixel), 1.0);
        }

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_src, src_tgt);
        let tm_output_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(tm_output_slot)
            .expect("tone map output should be retained on backend");
        let bytes_per_row = w * 8;
        let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut rb_enc = device.create_encoder("reinhard-readback");
        rb_enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
        rb_enc.commit_and_wait_completed();

        let ptr = buf.mapped_ptr().expect("shared buffer should expose mapped pointer");
        let px: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(px[0]).to_f32(),
            f16::from_bits(px[1]).to_f32(),
            f16::from_bits(px[2]).to_f32(),
            f16::from_bits(px[3]).to_f32(),
        ]
    }

    /// f16 storage quantization bound: ~0.1% relative at these
    /// magnitudes, so 5e-3 absolute is comfortably above rounding and
    /// far below any curve-selection or formula error.
    const TOL: f32 = 5e-3;

    fn assert_channel(label: &str, got: [f32; 4], expected: f32) {
        assert!(
            (got[0] - expected).abs() < TOL,
            "{label}: expected {expected}, got {}",
            got[0]
        );
        assert!((got[3] - 1.0).abs() < TOL, "{label}: alpha must pass through, got {}", got[3]);
    }

    #[test]
    fn extended_curve_regression_values() {
        // x = 0.5*2*1 = 1.0 → 1*(1+1/9)/2 = 0.555556
        assert_channel("extended x=1", run_tone_map_at(0.5, 2.0, 1.0, 0), 0.555_556);
        // x = 4 → 4*(1+4/9)/5 = 1.155556 (Extended preserves >1 highlights)
        assert_channel("extended x=4", run_tone_map_at(4.0, 1.0, 1.0, 0), 1.155_556);
    }

    #[test]
    fn simple_curve_regression_values() {
        // x = 1 → 0.5
        assert_channel("simple x=1", run_tone_map_at(0.5, 2.0, 1.0, 1), 0.5);
        // x = 4 → 0.8
        assert_channel("simple x=4", run_tone_map_at(4.0, 1.0, 1.0, 1), 0.8);
    }

    #[test]
    fn log_curve_hand_computed_values() {
        // x = 1 → log2(2)/log2(65) = 1/6.022368 = 0.166048
        assert_channel("log x=1", run_tone_map_at(0.5, 2.0, 1.0, 2), 0.166_048);
        // x = 4 → log2(5)/log2(65) = 2.321928/6.022368 = 0.385551
        assert_channel("log x=4", run_tone_map_at(4.0, 1.0, 1.0, 2), 0.385_551);
        // x = 64 (white point) → exactly 1.0
        assert_channel("log x=white", run_tone_map_at(64.0, 1.0, 1.0, 2), 1.0);
        // Faint-structure property — the reason the curve exists. Log
        // compresses the faint-to-hot RATIO: a 1-hit deposit keeps a
        // larger share of the display range relative to a 64-hit spot
        // than under Extended. (In absolute terms Log sits BELOW
        // Extended at tiny x — the win is relative visibility, not
        // absolute lift.)
        let log_ratio = run_tone_map_at(1.0, 1.0, 1.0, 2)[0] / run_tone_map_at(64.0, 1.0, 1.0, 2)[0];
        let ext_ratio = run_tone_map_at(1.0, 1.0, 1.0, 0)[0] / run_tone_map_at(64.0, 1.0, 1.0, 0)[0];
        assert!(
            log_ratio > ext_ratio * 2.0,
            "log must keep faint density ≥2× more visible relative to hot spots (log ratio {log_ratio}, extended ratio {ext_ratio})"
        );
    }
}
