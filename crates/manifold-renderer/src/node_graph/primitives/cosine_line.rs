//! `node.cosine_line` — the cosine transform along one axis of a lattice, one
//! workgroup per line (docs/FFT_WATER_SOLVER_DESIGN.md D9). Each line is
//! reordered, FFT'd in workgroup memory with barriers, and twiddled in one
//! dispatch, so it is the barriered class of the codegen exemption list
//! (docs/ADDING_PRIMITIVES.md, exemption class 1). Three in a row are the 3D
//! transform; axes 0 and 1 alone transform a stack of planes.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use super::cosine_spectrum::lattice_nodes;
use super::sort_particles_into_cells::float_param;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const WGSL: &str = include_str!("shaders/cosine_line.wgsl");
const MAX_LINE: u32 = 512;
const MAX_GROUPS_X: u32 = 65535;

/// Mirrors `Params` in `shaders/cosine_line.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CosineLineUniforms {
    nodes_x: u32,
    nodes_y: u32,
    nodes_z: u32,
    axis: u32,
    direction: u32,
    lines: u32,
    _pad0: u32,
    _pad1: u32,
}

fn int_param(params: &ParamValues, name: &str, max: f32) -> u32 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => v.round().clamp(0.0, max) as u32,
        _ => 0,
    }
}

crate::primitive! {
    name: CosineLine,
    type_id: "node.cosine_line",
    purpose: "Cosine transform of a lattice held in an Array<f32> (nodes_x/y/z nodes, node (i, j, k) at i + nx·(j + ny·k)) along one axis, one workgroup per line. Forward is the unnormalised DCT-II X[k] = Σ x[m] cos(π k (2m + 1) / 2N); inverse is the scaled DCT-III that undoes it exactly. The transformed length must be a power of two, 2 to 512.",
    inputs: {
        values: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 2.0, 1024.0),
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("direction"),
            label: "Direction",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Forward on axes 0, 1, 2, then node.cosine_poisson_divide, then inverse on axes 2, 1, 0 is the whole-box pressure solve with walls on every face. Axes 0 and 1 alone transform a stack of planes (the surface helper's views).",
    examples: [],
    picker: { label: "Cosine Line", category: Atom },
    summary: "Turns each line of a 3D grid into the strengths of its smooth wave patterns, or back again.",
    category: MathAndConvert,
    role: Map,
    aliases: ["dct", "dct-ii", "cosine transform", "idct"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        line_pipeline: Option<GpuComputePipeline> = None,
    },
}

impl Primitive for CosineLine {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = int_param(ctx.params, "axis", 2.0);
        let direction = int_param(ctx.params, "direction", 1.0);
        let Some(nodes) = lattice_nodes(ctx.params)
            .filter(|n| n[axis as usize].is_power_of_two() && n[axis as usize] <= MAX_LINE)
        else {
            ctx.error("Cosine Line: lengths must be even and the transformed one a power of two, 2 to 512".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self
            .line_pipeline
            .get_or_insert_with(|| gpu.device.create_compute_pipeline(WGSL, "cs_main", "node.cosine_line"));
        let (Some(values), Some(out)) = (ctx.inputs.array("values"), ctx.outputs.array("out")) else {
            return;
        };
        let total: u32 = nodes.iter().product();
        if u64::from(total) * 4 > values.size.min(out.size) {
            ctx.error(format!("Cosine Line: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let lines = total / nodes[axis as usize];
        let uniforms = CosineLineUniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            axis,
            direction,
            lines,
            _pad0: 0,
            _pad1: 0,
        };
        let groups = [lines.min(MAX_GROUPS_X), lines.div_ceil(MAX_GROUPS_X), 1];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: values, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            groups,
            "node.cosine_line",
        );
    }
}
