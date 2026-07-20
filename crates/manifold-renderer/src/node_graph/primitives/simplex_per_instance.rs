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

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

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
const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

crate::primitive! {
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
        _params: &crate::node_graph::effect_node::ParamValues,
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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; noise_common prepended via wgsl_includes for
            // simplex3d). simplex_per_instance.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.simplex_noise_per_copy standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.simplex_noise_per_copy",
            )
        });

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn simplex_per_instance_declares_vec2_in_and_f32_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        let f32_layout = ArrayType::of_known::<f32>();
        assert_eq!(SimplexPerInstance::TYPE_ID, "node.simplex_noise_per_copy");
        let uv_in = SimplexPerInstance::INPUTS
            .iter()
            .find(|p| p.name == "uv")
            .expect("uv input must exist");
        assert!(uv_in.required);
        assert_eq!(uv_in.ty, PortType::Array(vec2_layout));

        assert_eq!(SimplexPerInstance::OUTPUTS.len(), 1);
        assert_eq!(SimplexPerInstance::OUTPUTS[0].name, "out");
        assert_eq!(SimplexPerInstance::OUTPUTS[0].ty, PortType::Array(f32_layout));
    }

    #[test]
    fn simplex_per_instance_has_port_shadow_inputs_for_all_shaping_params() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let shaping_names = ["scale", "z", "offset_x", "offset_y"];
        for name in shaping_names {
            let port = SimplexPerInstance::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} is port-shadow, must be optional");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }
    }

    #[test]
    fn simplex_per_instance_out_capacity_follows_uv_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = SimplexPerInstance::new();
        let params = ParamValues::default();
        let inputs = [("uv", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(160_000),
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "other", &params, &inputs),
            None,
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SimplexPerInstance::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.simplex_noise_per_copy");
    }
}

