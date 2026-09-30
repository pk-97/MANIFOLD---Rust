//! `node.cosine_reorder` — the index shuffle on either side of the FFT in a
//! 3D cosine transform (docs/FFT_WATER_SOLVER_DESIGN.md D9). A per-element
//! gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cosine_spectrum::{AXES_PARAM, lattice_nodes, transform_axes};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ReorderUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    direction: i32,
    axes: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: CosineReorder,
    type_id: "node.cosine_reorder",
    purpose: "Shuffle a lattice held in an Array<f32> (nodes_x/y/z nodes, node (i, j, k) at i + nx·(j + ny·k), every transformed length even) so a cosine transform becomes a plain FFT: forward puts the even entries in order then the odd entries reversed, on every transformed axis (x, y and z with axes 3; x and y with axes 2); inverse undoes it. The first and last step of node.fft_3d-based cosine transforms.",
    inputs: {
        values: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 1.0, 4096.0),
        ParamDef {
            name: Cow::Borrowed("direction"),
            label: "Direction",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        AXES_PARAM,
    ],
    depth_rule: Terminal,
    composition_notes: "Forward cosine transform: cosine_reorder (direction 0) → fft_3d → cosine_spectrum. Inverse: cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1). Lengths must match the FFT plan.",
    examples: [],
    picker: { label: "Cosine Reorder", category: Atom },
    summary: "Shuffles a 3D grid of numbers into the order a cosine transform needs, or back again.",
    category: MathAndConvert,
    role: Map,
    aliases: ["dct reorder", "makhoul permutation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cosine_reorder_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for CosineReorder {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = lattice_nodes(ctx.params) else {
            ctx.error("Cosine Reorder: every transformed length must be even, 2 to 1024".to_string());
            return;
        };
        let direction = match ctx.params.get("direction") {
            Some(ParamValue::Float(d)) => d.round().clamp(0.0, 1.0) as i32,
            _ => 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(out)) = (ctx.inputs.array("values"), ctx.outputs.array("out")) else {
            return;
        };
        let total: u32 = nodes.iter().product();
        if u64::from(total) * 4 > values.size.min(out.size) {
            ctx.error(format!("Cosine Reorder: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = ReorderUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            direction,
            axes: transform_axes(ctx.params) as i32,
            dispatch_count: total,
            _pad0: 0,
            _pad1: 0,
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
            "node.cosine_reorder",
        );
    }
}
