//! `node.flow_lines` — Line Integral Convolution: walk N steps both
//! directions along a normalised velocity field, weighted-accumulate
//! `source.r` along the path. Classic flow visualisation atom.
//!
//! Two common source choices:
//!   - Hash noise (`node.noise` Random) → streamline patterns.
//!   - Derived scalar (e.g. `length(velocity)` via `node.vector_length`)
//!     → flow-aligned intensity.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LicUniforms {
    steps: u32,
    dt: f32,
    _pad0: f32,
    _pad1: f32,
}

crate::primitive! {
    name: LicIntegrate,
    type_id: "node.flow_lines",
    purpose: "Line Integral Convolution. For each pixel, walks N steps forward and N steps backward along the normalised velocity field, weighted-accumulating `source.r` along the path. Output = weighted_sum / total_weight in R; GBA = (0, 0, 1). Steps capped at 64. The classic flow-visualisation atom: pair `source` with a hash-noise texture for streamlines (oily-fluid Lines), or with a derived heightmap for flow-aligned intensity (oily-fluid Flow Field).",
    inputs: {
        source: Texture2D required,
        velocity: Texture2D required,
        steps: ScalarF32 optional,
        dt: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("steps"),
            label: "Steps",
            ty: ParamType::Int,
            default: ParamValue::Float(16.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("dt"),
            label: "Δt (pixels/step)",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.1, 16.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Steps × dt = effective LIC half-length in pixels (oily Flow Field uses 16×2 ≈ 32px streaks; Lines uses 20×1.5 ≈ 30px streaks). Velocity is normalised per-step (direction-only) — the magnitude scales the *advection rate* through `node.texture_advect`, not the LIC walk length. Output may exceed [0, 1] for sources with high local variance; pair downstream with `node.smoothstep` (thresholding into clean lines) or `node.tone_map` (smooth grade).",
    examples: [],
    picker: { label: "Flow Lines (LIC)", category: Atom },
    summary: "Smears noise along a flow field to reveal its streamlines, turning a vector field into a visible flow texture.",
    category: FieldsAndCoordinates,
    role: Filter,
    aliases: ["flow lines", "lic", "lic integrate", "streamlines", "flow viz"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/lic_integrate_body.wgsl"),
    input_access: [Gather, Gather],
}

impl Primitive for LicIntegrate {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let steps_f = match ctx.inputs.scalar("steps") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("steps") {
                Some(ParamValue::Float(f)) => *f,
                _ => 16.0,
            },
        };
        let steps = (steps_f.round() as u32).clamp(1, 64);
        let dt = match ctx.inputs.scalar("dt") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("dt") {
                Some(ParamValue::Float(f)) => *f,
                _ => 2.0,
            },
        };

        let Some(src) = ctx.inputs.texture_2d("source") else {
            return;
        };
        let Some(vel) = ctx.inputs.texture_2d("velocity") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `source` + `velocity` are both Gather inputs (sampled along the
            // body-walked streamline). Generated kernel binds uniform(0)/source(1)/
            // velocity(2)/samp(3)/dst(4). lic_integrate.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.flow_lines standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.flow_lines",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = LicUniforms {
            steps,
            dt,
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
                    texture: src,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: vel,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.flow_lines",
        );
    }
}
