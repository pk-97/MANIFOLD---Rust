//! `node.generate_duocylinder_vertices` — emit a parametric 4D torus
//! grid (duocylinder surface) into an `Array<Vec4Vertex>`.
//!
//! Vertex positions: (cos u, sin u, cos v, sin v) for (u, v) ∈ [0, 2π)²
//! with `grid_size` steps each. Index ordering is row-major in (u, v):
//! `idx = iu * grid_size + iv`. Matches `generators/duocylinder.rs`
//! bit-for-bit. Edge connectivity (neighbors in u and v with wrapping)
//! is the downstream consumer's concern.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const DUOCYLINDER_DEFAULT_GRID_SIZE: u32 = 24;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    grid_size: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: GenerateDuocylinderVertices,
    type_id: "node.generate_duocylinder_vertices",
    purpose: "Emit a parametric 4D torus (duocylinder surface) grid as Array<Vec4Vertex>. Positions are (cos u, sin u, cos v, sin v) for (u, v) sampled at grid_size steps each across [0, 2π). The 4D-side parametric-surface counterpart of node.generate_tesseract_vertices.",
    inputs: {},
    outputs: {
        vertices: Array(Vec4Vertex),
    },
    params: [
        ParamDef {
            name: "grid_size",
            label: "Grid Size",
            ty: ParamType::Int,
            default: ParamValue::Int(24),
            range: Some((4.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(576),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Output vertex count is grid_size² (default 24² = 576). max_capacity must accommodate that; smaller buffers truncate. Index order is row-major: idx = iu * grid_size + iv. Wireframe edges (u- and v-direction neighbors with wraparound) live downstream — consumer needs an edge-expansion stage before line rendering. To reproduce the legacy Duocylinder generator, wire rotate_4d → project_4d after this and apply the standard 4D torus edge connectivity.",
    examples: [],
    picker: { label: "Generate Duocylinder Vertices", category: Atom },
}

impl Primitive for GenerateDuocylinderVertices {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let grid_size = match ctx.params.get("grid_size") {
            Some(ParamValue::Int(n)) => (*n).max(2) as u32,
            _ => DUOCYLINDER_DEFAULT_GRID_SIZE,
        };

        let Some(dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let capacity = (dst.size / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_duocylinder_vertices.wgsl"),
                "cs_main",
                "node.generate_duocylinder_vertices",
            )
        });

        let uniforms = Uniforms {
            grid_size,
            capacity,
            _pad0: 0,
            _pad1: 0,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [grid_size.div_ceil(8), grid_size.div_ceil(8), 1],
            "node.generate_duocylinder_vertices",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_vec4_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType {
            item_size: std::mem::size_of::<Vec4Vertex>() as u32,
            item_align: std::mem::align_of::<Vec4Vertex>() as u32,
        };
        assert_eq!(
            GenerateDuocylinderVertices::TYPE_ID,
            "node.generate_duocylinder_vertices"
        );
        assert!(GenerateDuocylinderVertices::INPUTS.is_empty());
        assert_eq!(GenerateDuocylinderVertices::OUTPUTS.len(), 1);
        assert_eq!(
            GenerateDuocylinderVertices::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn registers_with_palette() {
        let prim = GenerateDuocylinderVertices::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(
            node.type_id().as_str(),
            "node.generate_duocylinder_vertices"
        );
    }
}
