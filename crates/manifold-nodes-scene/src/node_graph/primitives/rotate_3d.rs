//! `node.rotate_3d` — XYZ Euler rotation of an `Array<MeshVertex>`.
//!
//! WGSL port of `generators::generator_math::rotate_3d` — applies
//! rotations in X → Y → Z order to position and normal of each
//! vertex. Used by Wireframe-shaped graphs:
//! polytope_vertices → Rotate3D → (project) → render.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: the three Angle params (f32) in PARAMS
/// order, then the codegen-injected `dispatch_count` (= vertex capacity, the
/// guard). 4 words = 16 bytes. `active_count == capacity` (full pass), so no
/// inactive-collapse field is needed.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Rotate3DUniforms {
    angle_x: f32,
    angle_y: f32,
    angle_z: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: Rotate3D,
    type_id: "node.rotate_3d",
    purpose: "Apply XYZ Euler rotation to an Array<MeshVertex>. Rotates position and normal of each vertex in X → Y → Z order (matches generator_math::rotate_3d bit-for-bit). The 3D-equivalent of node.rotate_4d, used in Wireframe-shaped graphs: polytope_vertices → Rotate3D → (project) → render.",
    inputs: {
        in: Array(MeshVertex) required,
        // Port-shadows-param: when a wire is connected, the wired
        // value wins over the inline `angle_*` param. Lets the graph
        // drive angles from time / LFO / math nodes without lifting
        // each angle into a separate Value node.
        angle_x: ScalarF32 optional,
        angle_y: ScalarF32 optional,
        angle_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("angle_x"),
            label: "Angle X",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("angle_y"),
            label: "Angle Y",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("angle_z"),
            label: "Angle Z",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.0),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Active count = input buffer's vertex count (full pass-through; capacity-bound only). Output normals are rotated alongside positions so downstream rendering / lighting stays correct. For 4D rotation (Tesseract / Duocylinder) use node.rotate_4d.",
    examples: [],
    picker: { label: "Rotate 3D", category: Atom },
    summary: "Spins a 3D mesh around the X, Y, and Z axes. Wire an LFO or a beat into the angles to keep it turning.",
    category: Geometry3D,
    role: Filter,
    aliases: ["rotate 3d", "spin", "tumble", "euler"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rotate_3d_body.wgsl"),
}

impl Primitive for Rotate3D {
    /// Output `out` is sized to match input `in` — rotation is a
    /// vertex-by-vertex transform.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle_x = ctx.scalar_or_param("angle_x", 0.0);
        let angle_y = ctx.scalar_or_param("angle_y", 0.0);
        let angle_z = ctx.scalar_or_param("angle_z", 0.0);

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / vertex_size) as u32;
        let active_count = capacity;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let _ = active_count;

        let uniforms = Rotate3DUniforms {
            angle_x,
            angle_y,
            angle_z,
            dispatch_count: capacity,
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
                    buffer: in_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.rotate_3d",
        );
    }
}

