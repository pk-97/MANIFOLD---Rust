//! `node.sine_cosine` — per-pixel sin/cos/tan of `(input.rgb * freq + phase)`.
//!
//! Replaces the old standalone `node.sin_texture` and `node.cos_texture`
//! with a single primitive that switches on a `mode` enum (Sin / Cos / Tan).
//! Same input/output/param shape regardless of mode — authors don't pick
//! the wrong primitive or need to swap nodes when iterating on a pattern.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TrigUniforms {
    freq: f32,
    phase: f32,
    mode: u32,
    use_freq_tex: u32,
    use_phase_tex: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

pub const TRIG_MODES: &[&str] = &["Sin", "Cos", "Tan"];

crate::primitive! {
    name: TrigTexture,
    type_id: "node.sine_cosine",
    purpose: "Per-pixel trigonometric remap: out = trig_mode(input.rgb * freq + phase). Mode picks Sin / Cos / Tan; the rest of the wiring is identical so switching variants is one click. Tan output is clamped to ±32 to keep downstream shaders NaN/Inf-free. `freq` and `phase` can ALSO be driven per-pixel from optional texture inputs (`freq_tex` / `phase_tex` — R channel) — unlocks per-cell unique trig patterns (per-star twinkle, cellular flicker, etc.) when fed from a per-cell hash source like `node.voronoi_2d` (A channel) routed through `node.channel_mixer`.",
    inputs: {
        in: Texture2D required,
        // Port-shadows-param: wired scalars override the inline freq/phase.
        freq: ScalarF32 optional,
        phase: ScalarF32 optional,
        // Per-pixel texture-shadows for freq/phase. R channel is read.
        // When wired, takes precedence over scalar port-shadow + param.
        freq_tex: Texture2D optional,
        phase_tex: Texture2D optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("freq"),
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(std::f32::consts::TAU),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("phase"),
            label: "Phase",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("mode"),
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // Sin
            range: Some((0.0, (TRIG_MODES.len() - 1) as f32)),
            enum_values: TRIG_MODES,
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Default freq = 2π so a [0, 1] input completes one full cycle. Sin and Cos output range is [-1, 1]; Tan is clamped to ±32. For Lissajous-style XY compositions, pair two trig_texture nodes (one Sin, one Cos) driven from the same field. For per-cell unique twinkle / flicker patterns, wire `freq_tex` (per-pixel freq from a per-cell-stable source like voronoi A→R) and `phase_tex` from the same chain — each cell pulses at its own frequency and phase.",
    examples: [],
    picker: { label: "Sine / Cosine", category: Atom },
    summary: "Runs each value through sine, cosine, or tangent after scaling it. The building block for ripples and wave patterns out of a gradient.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["sine", "cosine", "trig texture", "sin", "cos", "wave"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/trig_texture_body.wgsl"),
}

impl Primitive for TrigTexture {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let freq = match ctx.inputs.scalar("freq") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("freq") {
                Some(ParamValue::Float(f)) => *f,
                _ => std::f32::consts::TAU,
            },
        };
        let phase = match ctx.inputs.scalar("phase") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("phase") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min((TRIG_MODES.len() - 1) as u32),
            Some(ParamValue::Float(f)) => (f.round() as u32).min((TRIG_MODES.len() - 1) as u32),
            _ => 0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let freq_tex = ctx.inputs.texture_2d("freq_tex");
        let phase_tex = ctx.inputs.texture_2d("phase_tex");
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // 3 coincident texture inputs (in + optional freq_tex/phase_tex). The
            // generated kernel binds uniform(0)/in(1)/freq_tex(2)/phase_tex(3)/
            // samp(4)/dst(5) — textures-then-sampler-then-output, which reorders the
            // hand layout (output was at 3). The injected use_freq_tex/use_phase_tex
            // flags select per-pixel texture vs scalar. trig_texture.wgsl is the
            // parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.sine_cosine standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.sine_cosine",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = TrigUniforms {
            freq,
            phase,
            mode,
            use_freq_tex: freq_tex.is_some() as u32,
            use_phase_tex: phase_tex.is_some() as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Bind input tex as the fallback for unwired freq_tex / phase_tex
        // slots — the shader's use_*_tex flag selects which path runs, so
        // the fallback texture is never sampled when its flag is 0.
        let freq_bind = freq_tex.unwrap_or(in_tex);
        let phase_bind = phase_tex.unwrap_or(in_tex);

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Texture { binding: 2, texture: freq_bind },
                GpuBinding::Texture { binding: 3, texture: phase_bind },
                GpuBinding::Sampler { binding: 4, sampler },
                GpuBinding::Texture { binding: 5, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.sine_cosine",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn trig_texture_declares_required_in_optional_scalars_and_optional_texture_shadows() {
        use crate::node_graph::ports::{PortType, ScalarType};
        assert_eq!(TrigTexture::TYPE_ID, "node.sine_cosine");
        let ins = TrigTexture::INPUTS;
        assert_eq!(ins.len(), 5);
        assert_eq!(ins[0].name, "in");
        assert!(ins[0].required);
        assert_eq!(ins[0].ty, PortType::Texture2D);
        assert_eq!(ins[1].name, "freq");
        assert!(!ins[1].required);
        assert_eq!(ins[1].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[2].name, "phase");
        assert!(!ins[2].required);
        assert_eq!(ins[2].ty, PortType::Scalar(ScalarType::F32));
        assert_eq!(ins[3].name, "freq_tex");
        assert!(!ins[3].required);
        assert_eq!(ins[3].ty, PortType::Texture2D);
        assert_eq!(ins[4].name, "phase_tex");
        assert!(!ins[4].required);
        assert_eq!(ins[4].ty, PortType::Texture2D);
        assert_eq!(TrigTexture::OUTPUTS.len(), 1);
    }

    #[test]
    fn trig_texture_has_freq_phase_mode_params() {
        let names: Vec<&str> = TrigTexture::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["freq", "phase", "mode"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TrigTexture::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.sine_cosine");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Hardware tests for the per-pixel freq_tex / phase_tex shadow
    //! contract: when wired, each pixel's freq (or phase) is read from
    //! the texture's R channel instead of the scalar uniform. This is
    //! what unlocks per-cell unique trig modulation for StarField's
    //! per-star twinkle and similar cellular-flicker patterns.
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use super::TrigTexture;
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

    /// Run TrigTexture with given inputs. `shadow` picks which optional
    /// texture port to wire (`Some("freq_tex")` or `Some("phase_tex")`)
    /// — when set, both `in` and the shadow port get wired to the same
    /// Source filled with `shadow_val` in R, while `in` gets cleared to
    /// `in_val` in R via a second source. When None, only `in` is wired.
    fn run_trig_with_freq_tex_shadow(in_val: f32, freq_tex_val: f32, scalar_freq: f32) -> f32 {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let in_src = g.add_node(Box::new(Source::new()));
        let ftex_src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(TrigTexture::new()));
        let sink = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(node, "freq", ParamValue::Float(scalar_freq)).unwrap();
        g.set_param(node, "phase", ParamValue::Float(0.0)).unwrap();
        g.set_param(node, "mode", ParamValue::Enum(0)).unwrap(); // Sin
        g.connect((in_src, "out"), (node, "in")).unwrap();
        g.connect((ftex_src, "out"), (node, "freq_tex")).unwrap();
        g.connect((node, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_in_src = output_resource(&plan, in_src, "out");
        let r_ftex_src = output_resource(&plan, ftex_src, "out");
        let r_out = output_resource(&plan, node, "out");
        let in_target = RenderTarget::new(&device, w, h, format, "trig-in");
        let ftex_target = RenderTarget::new(&device, w, h, format, "trig-ftex");
        let out_target = RenderTarget::new(&device, w, h, format, "trig-out");
        crate::clear_texture_committed(
            &device,
            &in_target.texture,
            [in_val as f64, in_val as f64, in_val as f64, 1.0],
            "trig-in-clear",
        );
        crate::clear_texture_committed(
            &device,
            &ftex_target.texture,
            [freq_tex_val as f64, 0.0, 0.0, 1.0],
            "trig-ftex-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_in_src, in_target);
        backend.pre_bind_texture_2d(r_ftex_src, ftex_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("trig-frame");
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
        let mut readback_enc = device.create_encoder("trig-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        f16::from_bits(pixels[0]).to_f32()
    }

    fn run_trig_with_phase_tex_shadow(in_val: f32, phase_tex_val: f32, scalar_phase: f32) -> f32 {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let in_src = g.add_node(Box::new(Source::new()));
        let ptex_src = g.add_node(Box::new(Source::new()));
        let node = g.add_node(Box::new(TrigTexture::new()));
        let sink = g.add_node(Box::new(FinalOutput::new()));
        // freq = 1.0 so x*freq = x and we isolate the phase contribution.
        g.set_param(node, "freq", ParamValue::Float(1.0)).unwrap();
        g.set_param(node, "phase", ParamValue::Float(scalar_phase))
            .unwrap();
        g.set_param(node, "mode", ParamValue::Enum(0)).unwrap();
        g.connect((in_src, "out"), (node, "in")).unwrap();
        g.connect((ptex_src, "out"), (node, "phase_tex")).unwrap();
        g.connect((node, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_in_src = output_resource(&plan, in_src, "out");
        let r_ptex_src = output_resource(&plan, ptex_src, "out");
        let r_out = output_resource(&plan, node, "out");
        let in_target = RenderTarget::new(&device, w, h, format, "trig-in");
        let ptex_target = RenderTarget::new(&device, w, h, format, "trig-ptex");
        let out_target = RenderTarget::new(&device, w, h, format, "trig-out");
        crate::clear_texture_committed(
            &device,
            &in_target.texture,
            [in_val as f64, in_val as f64, in_val as f64, 1.0],
            "trig-in-clear",
        );
        crate::clear_texture_committed(
            &device,
            &ptex_target.texture,
            [phase_tex_val as f64, 0.0, 0.0, 1.0],
            "trig-ptex-clear",
        );

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_in_src, in_target);
        backend.pre_bind_texture_2d(r_ptex_src, ptex_target);
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let mut native_enc = device.create_encoder("trig-frame");
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
        let mut readback_enc = device.create_encoder("trig-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        f16::from_bits(pixels[0]).to_f32()
    }

    /// When freq_tex is wired, per-pixel freq comes from texture.R
    /// instead of the scalar. Verify: in=1, freq_tex.r=π/2, scalar=999.
    /// Expected sin(1 * π/2 + 0) = 1.0. If the scalar leaked through,
    /// sin(1 * 999) would be some other value far from 1.0.
    #[test]
    fn freq_tex_shadow_overrides_scalar_freq() {
        let pi_over_2 = std::f32::consts::FRAC_PI_2;
        let out = run_trig_with_freq_tex_shadow(1.0, pi_over_2, 999.0);
        assert!(
            (out - 1.0).abs() < 0.02,
            "freq_tex should override scalar freq: got sin output {} expected ≈1.0",
            out,
        );
    }

    /// When phase_tex is wired, per-pixel phase comes from texture.R
    /// instead of the scalar. Verify: in=0 (so freq term cancels),
    /// phase_tex.r=π/2, scalar=999. Expected sin(0 + π/2) = 1.0.
    #[test]
    fn phase_tex_shadow_overrides_scalar_phase() {
        let pi_over_2 = std::f32::consts::FRAC_PI_2;
        let out = run_trig_with_phase_tex_shadow(0.0, pi_over_2, 999.0);
        assert!(
            (out - 1.0).abs() < 0.02,
            "phase_tex should override scalar phase: got sin output {} expected ≈1.0",
            out,
        );
    }
}
