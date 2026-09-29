//! `node.count_surface_triangles` — marching-cubes triangles per lattice cell
//! (GPU_FLUID_SURFACE_DESIGN.md D16). The first of three surface atoms: count,
//! running total, emit. A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

pub(crate) const MARCHING_CUBES_COMMON: &str = include_str!("shaders/marching_cubes_common.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CountUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: CountSurfaceTriangles,
    type_id: "node.count_surface_triangles",
    purpose: "For each cell of a level-set lattice (nodes_x/y/z nodes, cells indexed i + cx·(j + cy·k)), the number of marching-cubes triangles its surface crossing needs: 0 to 5. Negative values are inside.",
    inputs: {
        levelset: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        counts: Array(u32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire nodes_x/y/z from node.particle_volume's volume_nodes_x/y/z. Feed counts into node.running_total, then the running total and the same level set into node.volume_surface_mesh. Slots past the last cell hold 0, so the running total can run over the whole array.",
    examples: [],
    picker: { label: "Count Surface Triangles", category: Atom },
    summary: "Works out how many triangles each small cube of the liquid's surface needs, the first step of building its mesh.",
    category: Geometry3D,
    role: Filter,
    aliases: ["marching cubes count", "classify cells", "isosurface count"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/count_surface_triangles_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [MARCHING_CUBES_COMMON],
}

impl Primitive for CountSurfaceTriangles {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "counts")
            .then(|| inputs.iter().find(|(name, _)| *name == "levelset").map(|&(_, n)| n))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(levelset), Some(counts)) = (ctx.inputs.array("levelset"), ctx.outputs.array("counts")) else {
            return;
        };
        let node_total: u64 = nodes.iter().map(|&n| n.max(0.0) as u64).product();
        if nodes.iter().all(|&n| n >= 2.0) && node_total > levelset.size / 4 {
            ctx.error(format!(
                "Count Surface Triangles: a {}×{}×{} lattice is larger than its level set",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let capacity = (counts.size / 4) as u32;
        if capacity == 0 {
            return;
        }
        let uniforms = CountUniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            dispatch_count: capacity,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: levelset, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: counts, offset: 0 },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.count_surface_triangles",
        );
    }
}
