//! `node.simplex_noise_per_copy` — sample 3D simplex noise at each
//! UV position in an `Array<vec2<f32>>`, emit `Array<f32>`.
//!
//! Per-instance counterpart to `node.noise` — which
//! samples noise per-pixel into a Texture2D. This primitive samples
//! per buffer slot into an Array<f32>, the right shape for driving
//! per-instance state in mesh-instancing pipelines (per-particle
//! displacement, per-cube radius, per-stem height noise).
//!
//! Uses the same Ashima 3D simplex implementation as the rest of
//! the renderer's generator shaders (`noise_common.wgsl`), which is
//! prepended at pipeline-creation time. Bit-exact parity with any
//! legacy generator that calls `simplex3d(...)` from that file.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
/// Generated-codegen uniform layout: scalar params in PARAMS order (`scale`,
/// `z`, `offset_x`, `offset_y`) then the codegen-injected `dispatch_count` (=
/// element count, the guard), padded to a 16-byte multiple. 5 words + 3 pad = 32 B.
struct Uniforms {
    scale: f32,
    z: f32,
    offset_x: f32,
    offset_y: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// `noise_common.wgsl` prepended to the primitive shader at pipeline
/// creation — same pattern as the legacy `DigitalPlantsGenerator`.
/// Sharing the exact source file guarantees bit-exact parity with
/// any other shader that samples `simplex3d` from this library.
const NOISE_COMMON: &str = manifold_node_engine::gpu::shader_sources::NOISE_COMMON_WGSL;

manifold_node_engine::primitive! {
    name: SimplexPerInstance,
    type_id: "node.simplex_noise_per_copy",
    purpose: "Sample 3D Ashima simplex noise at each UV in an Array<vec2<f32>>, emit Array<f32>. Per-instance counterpart to node.noise (which samples per-pixel into a Texture2D). For each idx: out[idx] = simplex3d(vec3(uv[idx] * scale + offset, z)). All four shaping inputs (scale / z / offset_x / offset_y) are port-shadow-param so a time wire can drive `z` (animated noise field) or an LFO can pan `offset_*` (scrolling noise) without dragging extra Value nodes in.",
    inputs: {
        uv: Array([f32; 2]) required,
        scale: ScalarF32 optional,
        z: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("scale"),
            label: "Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z"),
            label: "Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_x"),
            label: "Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("offset_y"),
            label: "Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the input `uv` array (one noise sample per UV). `scale` is the same notion of frequency as in node.noise: ~1 = one cell across the UV range, ~32 = fine grain. Drive `z` from a time wire to animate the noise; drive `offset_*` from an LFO to pan. Bit-exact with `simplex3d(...)` from noise_common.wgsl — same source file is prepended at pipeline creation.",
    examples: [],
    picker: { label: "Simplex Noise (per copy)", category: Atom },
    summary: "Gives every copy its own simplex-noise value, a smooth random number per copy for varying the look across a field.",
    category: Particles2D,
    role: Filter,
    aliases: ["simplex noise", "simplex per instance", "per copy", "variation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/simplex_per_instance_body.wgsl"),
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for SimplexPerInstance {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "uv")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = ctx.scalar_or_param("scale", 4.0);
        let z = ctx.scalar_or_param("z", 0.0);
        let offset_x = ctx.scalar_or_param("offset_x", 0.0);
        let offset_y = ctx.scalar_or_param("offset_y", 0.0);

        let Some(uv_buf) = ctx.inputs.array("uv") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let vec2_size = std::mem::size_of::<[f32; 2]>() as u64;
        let f32_size = std::mem::size_of::<f32>() as u64;
        let in_capacity = (uv_buf.size / vec2_size) as u32;
        let out_capacity = (out_buf.size / f32_size) as u32;
        let count = in_capacity.min(out_capacity);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = Uniforms {
            scale,
            z,
            offset_x,
            offset_y,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: uv_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.simplex_noise_per_copy",
        );
    }
}

