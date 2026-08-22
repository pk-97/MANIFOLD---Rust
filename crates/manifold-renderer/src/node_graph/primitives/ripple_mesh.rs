//! `node.ripple_mesh` — per-vertex sinusoidal ripple along an `Array<MeshVertex>`.
//!
//! Per vertex: `pos += normal * amplitude * sin(dot(pos, dir) * frequency - time * speed)`,
//! where `dir` is the unit vector along the chosen `axis`, and `w` is the optional
//! per-vertex `weights` input (degrading to 1.0 past a short/unwired buffer). Normals,
//! uv, and tangent pass through unchanged. `time` is port-shadowed and defaults to
//! the playback clock when unwired.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const RIPPLE_AXES: &[&str] = &["X", "Y", "Z"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`amplitude`,
/// `frequency`, `speed`, `axis` Enum→u32, `time` f32), then the derived
/// `weights_len` (u32), then the codegen-injected `dispatch_count`, padded to a
/// 16-byte multiple. 7 words + 1 pad = 32 bytes. Matches
/// `standalone_for_spec::<RippleMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RippleUniforms {
    amplitude: f32,
    frequency: f32,
    speed: f32,
    axis: u32,
    time: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: RippleMesh,
    type_id: "node.ripple_mesh",
    purpose: "Per-vertex sinusoidal ripple of an Array<MeshVertex>. pos += normal * amplitude * sin(dot(pos, dir) * frequency - time * speed), where dir is the unit vector along the chosen axis. `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Normals, uv, and tangent pass through unchanged. `time` is port-shadowed and defaults to the playback clock when unwired.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        amplitude: ScalarF32 optional,
        frequency: ScalarF32 optional,
        speed: ScalarF32 optional,
        time: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amplitude"),
            label: "Amplitude",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("frequency"),
            label: "Frequency",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 256.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("speed"),
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1), // Y
            range: Some((0.0, (RIPPLE_AXES.len() - 1) as f32)),
            enum_values: RIPPLE_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("time"),
            label: "Time",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'ripple' deformer: waves travel along the surface normal, phase-driven by position along the chosen axis. Wire node.mesh_ramp's `weights` output to restrict the ripple to a region. Drive `time` from a beat ramp or leave it unwired for continuous playback-clock animation.",
    examples: [],
    picker: { label: "Ripple", category: Atom },
    summary: "Pushes every vertex along its normal by a sine wave indexed by position along an axis, making a mesh ripple like water or sheet metal.",
    category: Geometry3D,
    role: Filter,
    aliases: ["ripple", "ripple mesh", "wave", "sine displace"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/ripple_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable. `weights_len` is a frame-derived uniform; `time` is
    // frame-derived when unwired.
    derived_uniforms: ["weights_len:u32"],
    frame_time_inputs: ["time"],
}

// Per-frame recompute for a FUSED region's `time` field — `run()` packs
// `ctx.time.seconds.0` into the `time` uniform when the input is unwired.
inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.ripple_mesh",
        recompute: |ctx| Some(vec![ctx.frame.seconds.0 as f32]),
    }
}

impl Primitive for RippleMesh {
    /// Output `out` is sized to match input `in` — ripple displacement is a
    /// per-vertex transform, no expansion.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amplitude = ctx.scalar_or_param("amplitude", 0.0);
        let frequency = ctx.scalar_or_param("frequency", 1.0);
        let speed = ctx.scalar_or_param("speed", 1.0);
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min((RIPPLE_AXES.len() - 1) as u32),
            _ => 1,
        };
        let time = match ctx.inputs.scalar("time") {
            Some(ParamValue::Float(f)) => f,
            _ => ctx.time.seconds.0 as f32,
        };

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / vertex_size) as u32;
        let count = in_count.min(out_count);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path: the runtime kernel is generated from `wgsl_body`
            // so this atom stays pointwise/fusable in the graph compiler.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.ripple_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.ripple_mesh",
            )
        });

        let uniforms = RippleUniforms {
            amplitude,
            frequency,
            speed,
            axis,
            time,
            weights_len,
            dispatch_count: count,
            _pad0: 0,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.ripple_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn ripple_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(RippleMesh::TYPE_ID, "node.ripple_mesh");

        let in_port = RippleMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = RippleMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        for name in ["amplitude", "frequency", "speed", "time"] {
            let port = RippleMesh::INPUTS
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} port-shadow input must exist"));
            assert!(!port.required, "{name} should be optional (port-shadow)");
            assert_eq!(port.ty, PortType::Scalar(ScalarType::F32));
        }

        let axis_param = RippleMesh::PARAMS.iter().find(|p| p.name == "axis").unwrap();
        assert_eq!(axis_param.ty, ParamType::Enum);

        assert_eq!(RippleMesh::OUTPUTS.len(), 1);
        assert_eq!(RippleMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn ripple_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = RippleMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = RippleMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.ripple_mesh");
    }
}
