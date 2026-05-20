//! `node.generate_tesseract_vertices` — emit the 16 corner vertices of
//! a 4D unit hypercube (tesseract) into an `Array<Vec4Vertex>`.
//!
//! The 4D-side counterpart of [`crate::node_graph::primitives::GeneratePlatonicSolid`].
//! Output is the unique vertex set (16 corners); edge connectivity for
//! wireframe rendering is the downstream consumer's concern — the
//! 32-edge topology for a tesseract is the canonical bit-flip pattern
//! (connect i to i^1, i^2, i^4, i^8 where j > i).

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::Vec4Vertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub const TESSERACT_VERTEX_COUNT: u32 = 16;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: GenerateTesseractVertices,
    type_id: "node.generate_tesseract_vertices",
    purpose: "Emit the 16 corner vertices of a 4D unit hypercube (tesseract) as an Array<Vec4Vertex>. The 4D-side vertex-set building block for Tesseract-shaped graphs — feed downstream into node.rotate_4d → node.project_4d → a line renderer. Edge connectivity (32 edges, bit-flip pattern: connect i to i^1/2/4/8 where j>i) is downstream concern.",
    inputs: {},
    outputs: {
        vertices: Array(Vec4Vertex),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(16),
            range: Some((16.0, 4096.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Vertex coordinates are (sign(i&1), sign(i&2), sign(i&4), sign(i&8)). max_capacity=16 fits exactly; larger buffers leave trailing slots zeroed. The legacy Tesseract generator's render path applies the rotation/projection/line-segment expansion downstream — wire rotate_4d, project_4d, and a line renderer after this primitive to reproduce that pipeline.",
    examples: [],
    picker: { label: "Generate Tesseract Vertices", category: Atom },
}

impl Primitive for GenerateTesseractVertices {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
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
                include_str!("shaders/generate_tesseract_vertices.wgsl"),
                "cs_main",
                "node.generate_tesseract_vertices",
            )
        });

        let uniforms = Uniforms {
            capacity,
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
                    buffer: dst,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.generate_tesseract_vertices",
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
            GenerateTesseractVertices::TYPE_ID,
            "node.generate_tesseract_vertices"
        );
        assert!(GenerateTesseractVertices::INPUTS.is_empty());
        assert_eq!(GenerateTesseractVertices::OUTPUTS.len(), 1);
        assert_eq!(
            GenerateTesseractVertices::OUTPUTS[0].ty,
            PortType::Array(layout)
        );
    }

    #[test]
    fn registers_with_palette() {
        let prim = GenerateTesseractVertices::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_tesseract_vertices");
    }
}
