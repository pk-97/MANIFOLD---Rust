//! `node.rotate_4d` — apply a 4D rotation (XY, ZW, XW planes) to
//! an `Array<Vec4Vertex>` stream.
//!
//! Phase B of `BUFFER_PORT_PLAN`. Mirrors
//! `generators::generator_math::rotate_4d` so the behaviour is
//! bit-identical to Tesseract / Duocylinder / Wireframe when
//! wired with the same base verts. The transform primitive in
//! the mesh-family triad: producer → Rotate4D → renderer.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::mesh::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: the three Angle params (f32) in PARAMS
/// order, then the codegen-injected `dispatch_count` (= vertex capacity, the
/// guard). 4 words = 16 bytes. `active_count == capacity` (full pass).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RotateUniforms {
    angle_xy: f32,
    angle_zw: f32,
    angle_xw: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: Rotate4D,
    type_id: "node.rotate_4d",
    purpose: "Apply 4D rotation (XY, ZW, XW planes) to an Array<Vec4Vertex>. Matches generator_math::rotate_4d bit-for-bit. The transform stage of the 4D wireframe pipeline: producer → Rotate4D → renderer.",
    inputs: {
        in: Array(Vec4Vertex) required,
        // Port-shadows-param: when a wire is connected, the wired
        // value wins over the inline `angle_*` param. Lets the graph
        // drive angles from time / LFO / math nodes without lifting
        // each angle into a separate Value node. Mirrors rotate_3d.
        angle_xy: ScalarF32 optional,
        angle_zw: ScalarF32 optional,
        angle_xw: ScalarF32 optional,
    },
    outputs: {
        out: Array(Vec4Vertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("angle_xy"),
            label: "Angle XY",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.6),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("angle_zw"),
            label: "Angle ZW",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.4),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("angle_xw"),
            label: "Angle XW",
            ty: ParamType::Angle,
            default: ParamValue::Float(0.25),
            range: Some((-std::f32::consts::TAU, std::f32::consts::TAU)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Defaults match Tesseract's preset (0.6, 0.4, 0.25). Wire angle_* through Math nodes for time-based tumble. Active-count uses the *input* buffer's item count — output writes the same N items.",
    examples: [],
    picker: { label: "Rotate 4D", category: Atom },
    summary: "Spins 4D geometry through its rotation planes, the move that makes a tesseract appear to turn inside out.",
    category: Geometry3D,
    role: Filter,
    aliases: ["rotate 4d", "4d spin", "hyperrotation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/rotate_4d_body.wgsl"),
}

impl Primitive for Rotate4D {
    /// Output `out` is sized to match input `in` — rotation is a
    /// vertex-by-vertex transform.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "out" {
            input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let angle_xy = ctx.scalar_or_param("angle_xy", 0.6);
        let angle_zw = ctx.scalar_or_param("angle_zw", 0.4);
        let angle_xw = ctx.scalar_or_param("angle_xw", 0.25);

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let item_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let capacity = (in_buf.size.min(out_buf.size) / item_size) as u32;

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = RotateUniforms {
            angle_xy,
            angle_zw,
            angle_xw,
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
            "node.rotate_4d",
        );
    }
}

