//! `node.cosine_spectrum` — the twiddle stage that finishes a 3D cosine
//! transform built on one real FFT (docs/FFT_WATER_SOLVER_DESIGN.md D9). A
//! per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LatticeUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    axes: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

/// The `axes` param of the cosine-transform atoms: 3 transforms x, y and z;
/// 2 transforms x and y of every z slice on its own (a batch of planes).
pub(super) const AXES_PARAM: ParamDef = ParamDef {
    name: Cow::Borrowed("axes"),
    label: "Axes",
    ty: ParamType::Int,
    default: ParamValue::Float(3.0),
    range: Some((2.0, 3.0)),
    enum_values: &[],
};

pub(crate) fn transform_axes(params: &ParamValues) -> u32 {
    match params.get("axes") {
        Some(ParamValue::Float(a)) if a.round() == 2.0 => 2,
        _ => 3,
    }
}

/// Lattice lengths from the params. Every transformed length is even, 2 to
/// 1024; a batched z (axes 2) is any count from 1 to 4096.
pub(crate) fn lattice_nodes(params: &ParamValues) -> Option<[u32; 3]> {
    lattice_nodes_with(params, transform_axes(params))
}

pub(crate) fn lattice_nodes_with(params: &ParamValues, axes: u32) -> Option<[u32; 3]> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| match params.get(name) {
        Some(ParamValue::Float(n)) => n.round() as i64,
        _ => 64,
    });
    legal_lengths(nodes, axes)
}

/// Whether a box solve can transform a lattice of `cells`: the rule every
/// 3D cosine transform atom applies to its lengths.
pub(super) fn transformable_cells(cells: [u32; 3]) -> bool {
    legal_lengths(cells.map(i64::from), 3).is_some()
}

fn legal_lengths(nodes: [i64; 3], axes: u32) -> Option<[u32; 3]> {
    let batched = axes == 2;
    let valid = |axis: usize, n: i64| {
        if axis == 2 && batched { (1..=4096).contains(&n) } else { (2..=1024).contains(&n) && n % 2 == 0 }
    };
    nodes.iter().enumerate().all(|(axis, &n)| valid(axis, n)).then(|| nodes.map(|n| n as u32))
}

/// Entries in the half spectrum of a lattice: nx/2 + 1 along x.
pub(crate) fn half_spectrum_len(nodes: [u32; 3]) -> u32 {
    (nodes[0] / 2 + 1) * nodes[1] * nodes[2]
}

crate::primitive! {
    name: CosineSpectrum,
    type_id: "node.cosine_spectrum",
    purpose: "Unnormalised cosine transform (DCT-II) of a lattice, finished from the half spectrum node.fft_3d made of its node.cosine_reorder'd values: X[k] = Σ_n x[n] Π cos(π k (2n + 1) / 2N) over the transformed axes. Axes 3 transforms x, y and z (four gathers per coefficient); axes 2 transforms x and y of every z slice on its own (two gathers). Lattice nodes_x/y/z, node (i, j, k) at i + nx·(j + ny·k), every transformed length even.",
    inputs: {
        spectrum: Array([f32; 2]) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 1.0, 4096.0),
        AXES_PARAM,
    ],
    depth_rule: Terminal,
    composition_notes: "cosine_reorder (direction 0) → fft_3d → cosine_spectrum is the forward cosine transform. It diagonalises the cell-centred Laplacian with walls on every face (node.cosine_poisson_divide); with axes 2 it diagonalises the per-plane operator node.cosine_surface_scale applies. Every atom of a transform takes the same axes. Undo with cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1).",
    examples: [],
    picker: { label: "Cosine Spectrum", category: Atom },
    summary: "Finishes a 3D cosine transform, turning a grid into the strengths of its smooth wave patterns.",
    category: MathAndConvert,
    role: Map,
    aliases: ["dct", "dct-ii", "cosine transform"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cosine_spectrum_body.wgsl"),
    input_access: [BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS },
}

/// The lattice params whose product is a lattice atom's node count.
pub(super) const LATTICE_PARAMS: [&str; 3] = ["nodes_x", "nodes_y", "nodes_z"];

impl Primitive for CosineSpectrum {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| lattice_nodes(params).map(|n| n.iter().product())).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = lattice_nodes(ctx.params) else {
            ctx.error("Cosine Spectrum: every transformed length must be even, 2 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(spectrum), Some(out)) = (ctx.inputs.array("spectrum"), ctx.outputs.array("out")) else {
            return;
        };
        let total: u32 = nodes.iter().product();
        if u64::from(half_spectrum_len(nodes)) * 8 > spectrum.size || u64::from(total) * 4 > out.size {
            ctx.error(format!("Cosine Spectrum: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = LatticeUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            axes: transform_axes(ctx.params) as i32,
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
                GpuBinding::Buffer { binding: 1, buffer: spectrum, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [total.div_ceil(256), 1, 1],
            "node.cosine_spectrum",
        );
    }
}
