//! `node.cosine_reorder` — the index shuffle on either side of the FFT in a
//! 3D cosine transform (docs/FFT_WATER_SOLVER_DESIGN.md D9). A per-element
//! gather on the codegen path. It also moves a transform onto a window of a
//! larger lattice: forward gathers the window out, inverse spreads it back
//! with 0 around it (docs/FFT_WATER_SOLVER_DESIGN.md, P3c).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cosine_spectrum::{AXES_PARAM, lattice_nodes, live_lattice, transform_axes};
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
    origin_x: f32,
    origin_y: f32,
    origin_z: f32,
    outer_x: f32,
    outer_y: f32,
    outer_z: f32,
    dispatch_count: u32,
}

const ORIGIN_PARAMS: [&str; 3] = ["origin_x", "origin_y", "origin_z"];
const OUTER_PARAMS: [&str; 3] = ["outer_x", "outer_y", "outer_z"];

crate::primitive! {
    name: CosineReorder,
    type_id: "node.cosine_reorder",
    purpose: "Shuffle a lattice held in an Array<f32> (nodes_x/y/z nodes, node (i, j, k) at i + nx·(j + ny·k), every transformed length even) so a cosine transform becomes a plain FFT: forward puts the even entries in order then the odd entries reversed, on every transformed axis (x, y and z with axes 3; x and y with axes 2); inverse undoes it. The first and last step of node.fft_3d-based cosine transforms. With outer lengths set, the lattice is a window at origin inside a whole lattice of outer_x/y/z nodes: forward reads the window out of the whole lattice into a compact one, inverse writes the compact window back over the whole lattice with 0 outside it (outer 0: the window is the whole lattice).",
    inputs: {
        values: Array(f32) required,
        nodes_x: ScalarF32 optional,
        nodes_y: ScalarF32 optional,
        nodes_z: ScalarF32 optional,
        origin_x: ScalarF32 optional,
        origin_y: ScalarF32 optional,
        origin_z: ScalarF32 optional,
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
        float_param!("origin_x", "Origin X", 0.0, 0.0, 4096.0),
        float_param!("origin_y", "Origin Y", 0.0, 0.0, 4096.0),
        float_param!("origin_z", "Origin Z", 0.0, 0.0, 4096.0),
        float_param!("outer_x", "Outer X", 0.0, 0.0, 4096.0),
        float_param!("outer_y", "Outer Y", 0.0, 0.0, 4096.0),
        float_param!("outer_z", "Outer Z", 0.0, 0.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Forward cosine transform: cosine_reorder (direction 0) → fft_3d → cosine_spectrum. Inverse: cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1). Lengths must match the FFT plan. For a transform on a window, wire the same window lengths into every atom of the chain and the same origin into both reorders; the arrays stay sized for the whole lattice.",
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

/// Whole-lattice lengths: the outer params, or the window's own lengths where
/// an outer length is 0.
fn outer_nodes(params: &ParamValues, window: [u32; 3]) -> Option<[u32; 3]> {
    let mut outer = window;
    for (axis, &name) in OUTER_PARAMS.iter().enumerate() {
        let value = match params.get(name) {
            Some(ParamValue::Float(v)) => *v,
            _ => 0.0,
        };
        match whole_length(value)? {
            0 => {}
            length => outer[axis] = length,
        }
    }
    Some(outer)
}

/// The window's corner: whole numbers from 0.
fn window_origin(ctx: &EffectNodeContext<'_, '_>) -> Option<[u32; 3]> {
    let mut origin = [0; 3];
    for (axis, name) in ORIGIN_PARAMS.iter().enumerate() {
        origin[axis] = whole_length(ctx.scalar_or_param(name, 0.0))?;
    }
    Some(origin)
}

/// A whole number from 0 to 4096.
fn whole_length(value: f32) -> Option<u32> {
    ((0.0..=4096.0).contains(&value) && value.fract() == 0.0).then_some(value as u32)
}

fn product(n: [u32; 3]) -> u64 {
    n.iter().map(|&v| u64::from(v)).product()
}

impl Primitive for CosineReorder {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn params_refusal(&self, params: &ParamValues) -> Option<String> {
        let nodes = lattice_nodes(params)?;
        let Some(outer) = outer_nodes(params, nodes) else {
            return Some("Cosine Reorder: outer lengths must be whole numbers, 0 to 4096".to_string());
        };
        (0..3)
            .any(|a| outer[a] < nodes[a])
            .then(|| format!("Cosine Reorder: outer lengths {outer:?} must hold the {nodes:?} lattice"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = live_lattice(ctx) else {
            ctx.error("Cosine Reorder: every transformed length must be even, 2 to 1024".to_string());
            return;
        };
        let (Some(origin), Some(outer)) = (window_origin(ctx), outer_nodes(ctx.params, nodes)) else {
            ctx.error("Cosine Reorder: origin and outer lengths must be whole numbers, 0 to 4096".to_string());
            return;
        };
        if (0..3).any(|a| origin[a] + nodes[a] > outer[a]) {
            ctx.error(format!("Cosine Reorder: a {nodes:?} window at {origin:?} leaves the {outer:?} lattice"));
            return;
        }
        let direction = match ctx.params.get("direction") {
            Some(ParamValue::Float(d)) => d.round().clamp(0.0, 1.0) as i32,
            _ => 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(out)) = (ctx.inputs.array("values"), ctx.outputs.array("out")) else {
            return;
        };
        // Forward reads the whole lattice and writes the window; inverse the reverse.
        let (read, written) = if direction == 0 { (outer, nodes) } else { (nodes, outer) };
        if product(read) * 4 > values.size || product(written) * 4 > out.size {
            ctx.error(format!("Cosine Reorder: a {nodes:?} window of a {outer:?} lattice is larger than its arrays"));
            return;
        }
        let total = product(written) as u32;
        let uniforms = ReorderUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            direction,
            axes: transform_axes(ctx.params) as i32,
            origin_x: origin[0] as f32,
            origin_y: origin[1] as f32,
            origin_z: origin[2] as f32,
            outer_x: outer[0] as f32,
            outer_y: outer[1] as f32,
            outer_z: outer[2] as f32,
            dispatch_count: total,
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
