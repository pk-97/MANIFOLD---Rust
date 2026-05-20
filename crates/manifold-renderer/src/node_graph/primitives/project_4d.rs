//! `node.project_4d` — project an `Array<Vec4Vertex>` to
//! `Array<LinePoint>` via two-stage perspective (4D → 3D → 2D).
//!
//! Bit-exact WGSL port of `generators::generator_math::project_4d`.
//! Drives Tesseract / Duocylinder decomposition:
//! base verts → Rotate4D → Project4D → (line renderer).

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{LinePoint, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Project4DUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    proj_scale: f32,
    proj_dist: f32,
    _pad2: f32,
    _pad3: f32,
}

crate::primitive! {
    name: Project4D,
    type_id: "node.project_4d",
    purpose: "Project an Array<Vec4Vertex> to Array<LinePoint> via two-stage perspective (4D → 3D collapse with f = proj_dist / (proj_dist - w), then 3D → 2D with s = proj_dist / (proj_dist + p3z)). Bit-exact port of generator_math::project_4d. The 4D-equivalent of node.project_3d for Tesseract / Duocylinder decomposition.",
    inputs: {
        in: Array(Vec4Vertex) required,
    },
    outputs: {
        out: Array(LinePoint),
    },
    params: [
        ParamDef {
            name: "proj_scale",
            label: "Projection Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.25),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "proj_dist",
            label: "Projection Distance",
            ty: ParamType::Float,
            default: ParamValue::Float(3.0),
            range: Some((0.5, 100.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "PROJ_SCALE = 0.25 matches the legacy WireframeZoo / Tesseract default. The 4D → 3D collapse uses proj_dist as the W-axis camera distance; small values produce strong 4D distortion. Active count = input vertex buffer's capacity; output must be sized at least as large.",
    examples: [],
    picker: { label: "Project 4D", category: Atom },
}

impl Primitive for Project4D {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let proj_scale = match ctx.params.get("proj_scale") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.25,
        };
        let proj_dist = match ctx.params.get("proj_dist") {
            Some(ParamValue::Float(f)) => *f,
            _ => 3.0,
        };

        let Some(in_buf) = ctx.inputs.array("in") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let point_size = std::mem::size_of::<LinePoint>() as u64;
        let in_count = (in_buf.size / vertex_size) as u32;
        let out_capacity = (out_buf.size / point_size) as u32;
        let active_count = in_count.min(out_capacity);
        if active_count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/project_4d.wgsl"),
                "cs_main",
                "node.project_4d",
            )
        });

        let uniforms = Project4DUniforms {
            active_count,
            capacity: out_capacity,
            _pad0: 0,
            _pad1: 0,
            proj_scale,
            proj_dist,
            _pad2: 0.0,
            _pad3: 0.0,
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
            [out_capacity.div_ceil(64), 1, 1],
            "node.project_4d",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn project_4d_declares_vec4_in_and_linepoint_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec4_layout = ArrayType {
            item_size: std::mem::size_of::<Vec4Vertex>() as u32,
            item_align: std::mem::align_of::<Vec4Vertex>() as u32,
        };
        let point_layout = ArrayType {
            item_size: std::mem::size_of::<LinePoint>() as u32,
            item_align: std::mem::align_of::<LinePoint>() as u32,
        };
        assert_eq!(Project4D::TYPE_ID, "node.project_4d");
        assert_eq!(Project4D::INPUTS.len(), 1);
        assert_eq!(Project4D::INPUTS[0].ty, PortType::Array(vec4_layout));
        assert_eq!(Project4D::OUTPUTS.len(), 1);
        assert_eq!(Project4D::OUTPUTS[0].ty, PortType::Array(point_layout));
    }

    #[test]
    fn project_4d_has_scale_and_dist_params() {
        let names: Vec<&str> = Project4D::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["proj_scale", "proj_dist"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = Project4D::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.project_4d");
    }
}
