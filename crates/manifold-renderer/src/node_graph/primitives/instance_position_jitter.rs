//! `node.position_jitter` — add 3-axis 3D-simplex position
//! noise to each instance's `pos.xyz`, leave `pos.w` (scale) and
//! `rot_pad` unchanged.
//!
//! Per UV idx:
//! ```text
//! base = vec3(uv.x * frequency + time_uvx_drift,
//!             uv.y * frequency,
//!             z_coord)
//! pos += amplitude * vec3(
//!     simplex3d(base),
//!     simplex3d(base + vec3(axis_seed, 0, 0)),
//!     simplex3d(base + vec3(0, axis_seed, 0)),
//! )
//! ```
//!
//! Bit-exact reproduction of the legacy DigitalPlants detail (freq=20,
//! amp=0.01, no time, seed=100) and micro (freq=3, amp=0.02, time-driven,
//! seed=50) jitter patterns when given the matching params. Uses the
//! `simplex3d` from `noise_common.wgsl` (same file the legacy reads
//! from), so the noise samples agree byte-for-byte.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`frequency`,
/// `amplitude`, `time_uvx_drift`, `z_coord`, `axis_seed`), then the codegen-
/// injected `dispatch_count` (= element count, the guard), padded to a 16-byte
/// multiple. 6 words + 2 pad = 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    frequency: f32,
    amplitude: f32,
    time_uvx_drift: f32,
    z_coord: f32,
    axis_seed: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

const NOISE_COMMON: &str = include_str!("../../generators/shaders/noise_common.wgsl");

crate::primitive! {
    name: InstancePositionJitter,
    type_id: "node.position_jitter",
    purpose: "Add 3-axis 3D-simplex position noise to each InstanceTransform's pos.xyz, leaving scale and rotation unchanged. base = (uv.x*freq + time_uvx_drift, uv.y*freq, z_coord); pos += amp · (simplex(base), simplex(base + (seed,0,0)), simplex(base + (0,seed,0))). Generic — any instanced field that wants organic per-instance position wobble. Reproduces both legacy DigitalPlants detail- and micro-noise patterns when parameterised.",
    inputs: {
        instances: Array(InstanceTransform) required,
        uv: Array([f32; 2]) required,
        frequency: ScalarF32 optional,
        amplitude: ScalarF32 optional,
        time_uvx_drift: ScalarF32 optional,
        z_coord: ScalarF32 optional,
        axis_seed: ScalarF32 optional,
    },
    outputs: {
        instances: Array(InstanceTransform),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("frequency"),
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(10.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("amplitude"),
            label: "Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.01),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("time_uvx_drift"),
            label: "Time UV.x Drift",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("z_coord"),
            label: "Z Coord",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis_seed"),
            label: "Axis Seed",
            ty: ParamType::Float,
            default: ParamValue::Float(100.0),
            range: Some((0.0, 10_000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows the `instances` input. Drive `time_uvx_drift` and `z_coord` from time wires (typically `time * 0.2` and `time * 0.15` for slow drift) to animate the noise field. `axis_seed` decorrelates the three axis samples — pick any value large enough to land in a different noise cell (100 / 50 are the legacy DigitalPlants values for the detail and micro passes respectively). Pair upstream with node.grid_uv_field for the UV input. The original instance rotations are preserved verbatim — pair with node.rotation_jitter downstream if rotation jitter is also wanted.",
    examples: [],
    picker: { label: "Position Jitter", category: Atom },
    summary: "Adds a random offset to each copy's position with noise, so a perfect grid of copies looks more natural and scattered.",
    category: Particles2D,
    role: Filter,
    aliases: ["position jitter", "instance position jitter", "offset", "scatter", "noise"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/instance_position_jitter_body.wgsl"),
    wgsl_includes: [NOISE_COMMON],
}

impl Primitive for InstancePositionJitter {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "instances" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "instances")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let frequency = ctx.scalar_or_param("frequency", 10.0);
        let amplitude = ctx.scalar_or_param("amplitude", 0.01);
        let time_uvx_drift = ctx.scalar_or_param("time_uvx_drift", 0.0);
        let z_coord = ctx.scalar_or_param("z_coord", 0.0);
        let axis_seed = ctx.scalar_or_param("axis_seed", 100.0);

        let Some(uv_buf) = ctx.inputs.array("uv") else {
            return;
        };
        let Some(in_inst_buf) = ctx.inputs.array("instances") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("instances") else {
            return;
        };

        let vec2_size = std::mem::size_of::<[f32; 2]>() as u64;
        let inst_size = std::mem::size_of::<InstanceTransform>() as u64;
        let uv_cap = (uv_buf.size / vec2_size) as u32;
        let in_cap = (in_inst_buf.size / inst_size) as u32;
        let out_cap = (out_buf.size / inst_size) as u32;
        let count = uv_cap.min(in_cap).min(out_cap);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer
            // coincident path; noise_common prepended via wgsl_includes for
            // simplex3d). instance_position_jitter.wgsl (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B, migration scaffolding retired).
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.position_jitter standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.position_jitter",
            )
        });

        let uniforms = Uniforms {
            frequency,
            amplitude,
            time_uvx_drift,
            z_coord,
            axis_seed,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };

        // Generated binding order follows the INPUTS declaration (array inputs:
        // `instances` then `uv`), so bind instances at 1, uv at 2, output at 3.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: in_inst_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: uv_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.position_jitter",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn instance_position_jitter_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let inst_layout = ArrayType::of_known::<InstanceTransform>();
        let vec2_layout = ArrayType::of_known::<[f32; 2]>();
        assert_eq!(InstancePositionJitter::TYPE_ID, "node.position_jitter");

        let inst_in = InstancePositionJitter::INPUTS
            .iter()
            .find(|p| p.name == "instances")
            .unwrap();
        assert!(inst_in.required);
        assert_eq!(inst_in.ty, PortType::Array(inst_layout));

        let uv_in = InstancePositionJitter::INPUTS
            .iter()
            .find(|p| p.name == "uv")
            .unwrap();
        assert!(uv_in.required);
        assert_eq!(uv_in.ty, PortType::Array(vec2_layout));

        for name in [
            "frequency",
            "amplitude",
            "time_uvx_drift",
            "z_coord",
            "axis_seed",
        ] {
            let port = InstancePositionJitter::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        assert_eq!(InstancePositionJitter::OUTPUTS.len(), 1);
        assert_eq!(InstancePositionJitter::OUTPUTS[0].name, "instances");
        assert_eq!(
            InstancePositionJitter::OUTPUTS[0].ty,
            PortType::Array(inst_layout),
        );
    }

    #[test]
    fn instance_position_jitter_output_follows_instances_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = InstancePositionJitter::new();
        let params = ParamValues::default();
        let inputs = [("instances", 160_000_u32), ("uv", 160_000_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "instances", &params, &inputs),
            Some(160_000),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = InstancePositionJitter::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.position_jitter");
    }
}

