//! `node.cosine_poisson_divide` — solve the walled Poisson equation in cosine
//! space by dividing each coefficient by its Laplacian eigenvalue
//! (docs/FFT_WATER_SOLVER_DESIGN.md D3). A per-element atom on the codegen
//! path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cosine_spectrum::live_lattice;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DivideUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CosinePoissonDivide,
    type_id: "node.cosine_poisson_divide",
    purpose: "Divide each 3D cosine-transform coefficient (node.cosine_spectrum layout) by the eigenvalue of the cell-centred 7-point Laplacian with walls on every face, −4 Σ sin²(π k / 2N) / h². Between a forward and an inverse cosine transform this solves ∇²p = f exactly; the constant mode is zeroed, so f's mean is dropped and p has zero mean. Wired lengths run a smaller lattice in arrays sized for the params'.",
    inputs: {
        values: Array(f32) required,
        nodes_x: ScalarF32 optional,
        nodes_y: ScalarF32 optional,
        nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 2.0, 1024.0),
        float_param!("cell_size", "Cell Size", 1.0, 1e-6, 1e6),
    ],
    depth_rule: Terminal,
    composition_notes: "cosine_reorder → fft_3d → cosine_spectrum → cosine_poisson_divide → cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1) is the whole-box pressure solve of the FFT water solver.",
    examples: [],
    picker: { label: "Cosine Poisson Divide", category: Atom },
    summary: "Solves for pressure in a walled box by dividing each smooth wave by how strongly the box resists it.",
    category: MathAndConvert,
    role: Map,
    aliases: ["poisson solve", "spectral poisson", "inverse laplacian"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cosine_poisson_divide_body.wgsl"),
}

impl Primitive for CosinePoissonDivide {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = live_lattice(ctx) else {
            ctx.error("Cosine Poisson Divide: every length must be even, 2 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 1.0).max(1e-6);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(out)) = (ctx.inputs.array("values"), ctx.outputs.array("out")) else {
            return;
        };
        let total: u32 = nodes.iter().product();
        if u64::from(total) * 4 > values.size.min(out.size) {
            ctx.error(format!("Cosine Poisson Divide: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = DivideUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            dispatch_count: total,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: values, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [total.div_ceil(256), 1, 1],
            "node.cosine_poisson_divide",
        );
    }
}
