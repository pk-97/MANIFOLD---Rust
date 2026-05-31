//! `node.displace_mesh` — perturb the Y component of an
//! `Array<MeshVertex>` positions grid by sampling a height
//! Texture2D at each vertex's UV.
//!
//! New WGSL — the legacy MetallicGlass displacement is inline in
//! its vertex shader; this primitive lifts the operation into a
//! standalone compute kernel so the displaced grid can flow
//! through downstream primitives (notably TriangulateGrid →
//! Render3DMesh).
//!
//! Operates on row-major positions grids (one thread per vertex).
//! For triangulated meshes where the UV→vertex mapping isn't a
//! regular grid, route through this primitive *before*
//! TriangulateGrid.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplaceUniforms {
    cols: u32,
    rows: u32,
    capacity: u32,
    _pad0: u32,
    displacement: f32,
    height_bias: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: DisplaceMesh,
    type_id: "node.displace_mesh",
    purpose: "Perturb the Y component of an Array<MeshVertex> positions grid by sampling a height Texture2D at each vertex's UV. cols/rows describe the source grid topology; UV = (col / (cols-1), row / (rows-1)). For MetallicGlass-shaped graphs: GenerateGridMesh → DisplaceMesh → TriangulateGrid → Render3DMesh.",
    inputs: {
        in: Array(MeshVertex) required,
        height: Texture2D required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: "cols",
            label: "Columns",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "rows",
            label: "Rows",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "displacement",
            label: "Displacement",
            ty: ParamType::Float,
            default: ParamValue::Float(0.2),
            range: Some((-10.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "height_bias",
            label: "Height Bias",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "displaced_y = src.y + (height_sample.r - height_bias) * displacement. height_bias = 0.5 centers the displacement (matches MetallicGlass's behaviour where 0.5-luma maps to no displacement). displacement = 0.0 is pass-through. Bilinear texture sampling. Normals are passed through unchanged — the downstream TriangulateGrid recomputes them from displaced positions.",
    examples: [],
    picker: { label: "Push Mesh", category: Atom },
    summary: "Pushes a mesh's points up and down by reading a height image, turning a flat grid into bumpy terrain. The 3D version of a displacement.",
    category: Geometry3D,
    role: Filter,
    aliases: ["displace mesh", "push mesh", "height", "terrain"],
}

impl Primitive for DisplaceMesh {
    /// Output `out` is sized to match input `in` — displacement is a
    /// per-vertex transform, no expansion.
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
        let cols = match ctx.params.get("cols") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let rows = match ctx.params.get("rows") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let displacement = match ctx.params.get("displacement") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.2,
        };
        let height_bias = match ctx.params.get("height_bias") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.5,
        };

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(height) = ctx.inputs.texture_2d("height") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let capacity = (src.size.min(dst.size) / vertex_size) as u32;
        if capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/displace_mesh.wgsl"),
                "cs_main",
                "node.displace_mesh",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DisplaceUniforms {
            cols,
            rows,
            capacity,
            _pad0: 0,
            displacement,
            height_bias,
            _pad1: 0.0,
            _pad2: 0.0,
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
                    buffer: dst,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: height,
                },
                GpuBinding::Sampler {
                    binding: 4,
                    sampler,
                },
            ],
            [capacity.div_ceil(64), 1, 1],
            "node.displace_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn displace_mesh_declares_mesh_and_height_inputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(DisplaceMesh::TYPE_ID, "node.displace_mesh");
        assert_eq!(DisplaceMesh::INPUTS.len(), 2);
        assert_eq!(DisplaceMesh::INPUTS[0].name, "in");
        assert_eq!(DisplaceMesh::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(DisplaceMesh::INPUTS[1].name, "height");
        assert_eq!(DisplaceMesh::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(DisplaceMesh::OUTPUTS.len(), 1);
        assert_eq!(DisplaceMesh::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn displace_mesh_has_grid_and_displacement_params() {
        let names: Vec<&str> = DisplaceMesh::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["cols", "rows", "displacement", "height_bias"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = DisplaceMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.displace_mesh");
    }
}
