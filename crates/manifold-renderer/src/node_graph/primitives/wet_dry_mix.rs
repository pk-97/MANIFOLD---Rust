//! `node.wet_dry` — full-RGBA linear interpolation between
//! a `dry` and `wet` texture. Atomic building block extracted from
//! legacy `wet_dry_lerp_compute.wgsl`; used by every composite that
//! crossfades a processed result back over its source (Bloom,
//! Halation, Watercolor).
//!
//! Distinct from `node.mix` (Lerp mode): the port names `dry`/`wet`
//! make the dataflow direction explicit for composite authors, and the
//! shader carries only the lerp math — no mode switch overhead.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: WetDry,
    type_id: "node.wet_dry",
    purpose: "Crossfade a processed `wet` texture back over the original `dry` texture by a `wet_dry` factor [0,1]. At 0 returns dry unchanged; at 1 returns wet. RGBA-wide lerp.",
    inputs: {
        dry: Texture2D required,
        wet: Texture2D required,
        wet_dry: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("wet_dry"),
            label: "Wet/Dry",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "Prefer over Mix(Lerp) when wiring a processed branch back over an unprocessed source — the named ports make the intent self-documenting in composite graphs. The `wet_dry` input is an optional control wire: when wired, the scalar value overrides the same-named param for that frame.",
    examples: ["composite.bloom", "composite.halation", "composite.watercolor"],
    picker: { label: "Wet/Dry", category: Atom },
    summary: "Crossfades a processed image back over the original, so you can dial how much of an effect shows. At 0 you get the original, at 1 the full effect.",
    category: Composite,
    role: Filter,
    aliases: ["wet dry", "dry wet", "blend amount", "mix amount"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/wet_dry_mix_body.wgsl"),
}

pub const WET_DRY_TYPE_ID: &str = "node.wet_dry";

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WetDryMixUniforms {
    wet_dry: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Primitive for WetDry {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Wire wins, param is the fallback. Same convention as
        // FluidSim's `dye_color` port: an upstream scalar producer
        // (LFO, audio bridge, etc.) drives the knob continuously; the
        // declared param is the static value used when nothing is
        // wired.
        let wet_dry = match ctx.inputs.scalar("wet_dry") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("wet_dry") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };

        let Some(dry_tex) = ctx.inputs.texture_2d("dry") else {
            return;
        };
        let Some(wet_tex) = ctx.inputs.texture_2d("wet") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.wet_dry standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.wet_dry",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = WetDryMixUniforms {
            wet_dry,
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
                    texture: dry_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: wet_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.wet_dry",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU smoke tests. WetDry is a new primitive — no legacy
    //! effect to parity-check against directly — so we validate at the
    //! boundary values (wet_dry = 0 → dry, wet_dry = 1 → wet, wet_dry
    //! = 0.5 → exact half).

    

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

    use super::WetDry;

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

    fn run_wet_dry_at(dry_rgba: [f32; 4], wet_rgba: [f32; 4], wet_dry: f32) -> [f32; 4] {
        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let src_dry = g.add_node(Box::new(Source::new()));
        let src_wet = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(Box::new(WetDry::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.set_param(mix, "wet_dry", ParamValue::Float(wet_dry))
            .unwrap();
        g.connect((src_dry, "out"), (mix, "dry")).unwrap();
        g.connect((src_wet, "out"), (mix, "wet")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_dry = output_resource(&plan, src_dry, "out");
        let r_wet = output_resource(&plan, src_wet, "out");
        let dry_target = RenderTarget::new(&device, w, h, format, "test-dry");
        let wet_target = RenderTarget::new(&device, w, h, format, "test-wet");
        let mut native_enc = device.create_encoder("wet-dry-mix");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(
                &dry_target.texture,
                dry_rgba[0] as f64,
                dry_rgba[1] as f64,
                dry_rgba[2] as f64,
                dry_rgba[3] as f64,
            );
            gpu.clear_texture(
                &wet_target.texture,
                wet_rgba[0] as f64,
                wet_rgba[1] as f64,
                wet_rgba[2] as f64,
                wet_rgba[3] as f64,
            );
        }

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_dry, dry_target);
        backend.pre_bind_texture_2d(r_wet, wet_target);
        let mix_output_slot = Slot(backend.slot_count());

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(mix_output_slot)
            .expect("wet_dry output texture should be retained on backend");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("wet-dry-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
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

    #[test]
    fn wet_dry_zero_returns_dry() {
        let dry = [0.2, 0.4, 0.6, 0.8];
        let wet = [0.9, 0.1, 0.7, 1.0];
        let out = run_wet_dry_at(dry, wet, 0.0);
        let tol = 0.01;
        for c in 0..4 {
            assert!(
                (out[c] - dry[c]).abs() < tol,
                "channel {c}: {} != dry={} at wet_dry=0",
                out[c],
                dry[c]
            );
        }
    }

    #[test]
    fn wet_dry_one_returns_wet() {
        let dry = [0.2, 0.4, 0.6, 0.8];
        let wet = [0.9, 0.1, 0.7, 1.0];
        let out = run_wet_dry_at(dry, wet, 1.0);
        let tol = 0.01;
        for c in 0..4 {
            assert!(
                (out[c] - wet[c]).abs() < tol,
                "channel {c}: {} != wet={} at wet_dry=1",
                out[c],
                wet[c]
            );
        }
    }

    /// Drive `wet_dry` through a control wire (Value → WetDry.wet_dry)
    /// and assert the output matches the param-driven case at the same
    /// value. The param is deliberately set to 0.0 to prove the wire
    /// overrides — if the wire path weren't honoured, the output would
    /// be the pure `dry` colour.
    #[test]
    fn wet_dry_wired_scalar_overrides_param() {
        use super::super::Value as ValuePrimitive;

        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;
        let dry = [0.2_f32, 0.4, 0.6, 0.8];
        let wet = [0.8_f32, 0.0, 0.2, 1.0];

        let mut g = Graph::new();
        let src_dry = g.add_node(Box::new(Source::new()));
        let src_wet = g.add_node(Box::new(Source::new()));
        let amount = g.add_node(Box::new(ValuePrimitive::new()));
        let mix = g.add_node(Box::new(WetDry::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        // Param says 0.0 (pure dry) — the wire from Value(0.5) must win.
        g.set_param(mix, "wet_dry", ParamValue::Float(0.0)).unwrap();
        g.set_param(amount, "value", ParamValue::Float(0.5)).unwrap();
        g.connect((src_dry, "out"), (mix, "dry")).unwrap();
        g.connect((src_wet, "out"), (mix, "wet")).unwrap();
        g.connect((amount, "out"), (mix, "wet_dry")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_dry = output_resource(&plan, src_dry, "out");
        let r_wet = output_resource(&plan, src_wet, "out");
        let r_mix_out = output_resource(&plan, mix, "out");

        let dry_target = RenderTarget::new(&device, w, h, format, "test-dry");
        let wet_target = RenderTarget::new(&device, w, h, format, "test-wet");
        let mut native_enc = device.create_encoder("wet-dry-wire");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(
                &dry_target.texture,
                dry[0] as f64, dry[1] as f64, dry[2] as f64, dry[3] as f64,
            );
            gpu.clear_texture(
                &wet_target.texture,
                wet[0] as f64, wet[1] as f64, wet[2] as f64, wet[3] as f64,
            );
        }

        let out_target = RenderTarget::new(&device, w, h, format, "test-mix-out");
        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(r_dry, dry_target);
        backend.pre_bind_texture_2d(r_wet, wet_target);
        // Pin the mix output slot so it survives the executor's
        // post-step `release` — mirrors how the live renderer pre-binds
        // a chain output. `slot_for(r_mix_out)` after the frame would
        // return None otherwise.
        let mix_slot = backend.pre_bind_texture_2d(r_mix_out, out_target);

        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let tex = exec
            .backend()
            .texture_2d(mix_slot)
            .expect("wet_dry output texture retained on backend");

        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("wet-dry-wire-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf
            .mapped_ptr()
            .expect("shared buffer should expose mapped pointer");
        let pixels: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        let out_rgba = [
            f16::from_bits(pixels[0]).to_f32(),
            f16::from_bits(pixels[1]).to_f32(),
            f16::from_bits(pixels[2]).to_f32(),
            f16::from_bits(pixels[3]).to_f32(),
        ];

        // 0.5 wire-driven lerp = exact half.
        let tol = 0.01;
        for c in 0..4 {
            let expected = 0.5 * (dry[c] + wet[c]);
            assert!(
                (out_rgba[c] - expected).abs() < tol,
                "channel {c}: {} != avg={} (wire 0.5 should override param 0.0)",
                out_rgba[c], expected
            );
        }
    }

    #[test]
    fn wet_dry_half_returns_average() {
        let dry = [0.2, 0.4, 0.6, 0.8];
        let wet = [0.8, 0.0, 0.2, 1.0];
        let out = run_wet_dry_at(dry, wet, 0.5);
        let tol = 0.01;
        for c in 0..4 {
            let expected = 0.5 * (dry[c] + wet[c]);
            assert!(
                (out[c] - expected).abs() < tol,
                "channel {c}: {} != avg={} at wet_dry=0.5",
                out[c],
                expected
            );
        }
    }
}
