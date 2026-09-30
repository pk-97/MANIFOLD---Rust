//! `node.cosine_surface_scale` — the symbol of the six-view surface helper
//! (docs/FFT_WATER_SOLVER_DESIGN.md D4): each plane's cosine coefficients
//! scaled by sqrt(−Δ_s + q0²). A per-element atom on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cosine_spectrum::lattice_nodes_with;
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceScaleUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    lowest_wave: f32,
    offset: f32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: CosineSurfaceScale,
    type_id: "node.cosine_surface_scale",
    purpose: "Scale the 2D cosine coefficients of a stack of planes (node.cosine_spectrum with axes 2: nodes_x × nodes_y per plane, nodes_z planes) by sqrt(4 sin²(π kx / 2nx) / h² + 4 sin²(π ky / 2ny) / h² + q0²) − offset: the walled surface Laplacian's square root, floored by the lowest wave number q0, less a constant. Between a forward and an inverse plane transform it applies the surface part of the six-view pressure helper; offset 2/h takes out the helper's local term, which node.chart_spread adds back per entry.",
    inputs: {
        values: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Planes", 24.0, 1.0, 4096.0),
        float_param!("cell_size", "Cell Size", 1.0, 1e-6, 1e6),
        float_param!("lowest_wave", "Lowest Wave Number (rad/m)", 1.5707964, 0.0, 1e6),
        float_param!("offset", "Offset", 0.0, -1e6, 1e6),
    ],
    depth_rule: Terminal,
    composition_notes: "cosine_reorder → fft_3d → cosine_spectrum → cosine_surface_scale → cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1), every atom with axes 2. lowest_wave is 2π over the box length (1.571 for a 4 m box).",
    examples: [],
    picker: { label: "Cosine Surface Scale", category: Atom },
    summary: "Weights each smooth wave on a water surface by how sharply it bends, for the pressure solver's surface helper.",
    category: MathAndConvert,
    role: Map,
    aliases: ["surface symbol", "dirichlet to neumann", "half laplacian"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cosine_surface_scale_body.wgsl"),
}

impl Primitive for CosineSurfaceScale {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = lattice_nodes_with(ctx.params, 2) else {
            ctx.error("Cosine Surface Scale: plane lengths must be even, 2 to 1024".to_string());
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 1.0).max(1e-6);
        let lowest_wave = ctx.scalar_or_param("lowest_wave", 1.5707964).max(0.0);
        let offset = ctx.scalar_or_param("offset", 0.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(out)) = (ctx.inputs.array("values"), ctx.outputs.array("out")) else {
            return;
        };
        let total: u32 = nodes.iter().product();
        if u64::from(total) * 4 > values.size.min(out.size) {
            ctx.error(format!("Cosine Surface Scale: {nodes:?} planes are larger than their arrays"));
            return;
        }
        let uniforms = SurfaceScaleUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            lowest_wave,
            offset,
            dispatch_count: total,
            _pad0: 0,
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
            "node.cosine_surface_scale",
        );
    }
}
