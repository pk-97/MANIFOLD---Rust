//! `node.cosine_spectrum` — the twiddle stage that finishes a 3D cosine
//! transform built on one real FFT (docs/FFT_WATER_SOLVER_DESIGN.md D9). A
//! per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LatticeUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

/// Lattice lengths from the params; every one even and at least 2.
pub(super) fn lattice_nodes(params: &ParamValues) -> Option<[u32; 3]> {
    let nodes = ["nodes_x", "nodes_y", "nodes_z"].map(|name| match params.get(name) {
        Some(ParamValue::Float(n)) => n.round() as i64,
        _ => 64,
    });
    nodes.iter().all(|&n| n >= 2 && n % 2 == 0 && n <= 1024).then(|| nodes.map(|n| n as u32))
}

/// Entries in the half spectrum of a lattice: nx/2 + 1 along x.
pub(super) fn half_spectrum_len(nodes: [u32; 3]) -> u32 {
    (nodes[0] / 2 + 1) * nodes[1] * nodes[2]
}

crate::primitive! {
    name: CosineSpectrum,
    type_id: "node.cosine_spectrum",
    purpose: "Unnormalised 3D cosine transform (DCT-II on every axis) of a lattice, finished from the half spectrum node.fft_3d made of its node.cosine_reorder'd values: X[k] = Σ_n x[n] Π cos(π k (2n + 1) / 2N). Four gathers per coefficient. Lattice nodes_x/y/z, node (i, j, k) at i + nx·(j + ny·k), every length even.",
    inputs: {
        spectrum: Array([f32; 2]) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 2.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "cosine_reorder (direction 0) → fft_3d → cosine_spectrum is the forward cosine transform. It diagonalises the cell-centred Laplacian with walls on every face (node.cosine_poisson_divide). Undo with cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1).",
    examples: [],
    picker: { label: "Cosine Spectrum", category: Atom },
    summary: "Finishes a 3D cosine transform, turning a grid into the strengths of its smooth wave patterns.",
    category: MathAndConvert,
    role: Map,
    aliases: ["dct", "dct-ii", "cosine transform"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cosine_spectrum_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for CosineSpectrum {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| lattice_nodes(params).map(|n| n.iter().product())).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = lattice_nodes(ctx.params) else {
            ctx.error("Cosine Spectrum: every length must be even, 2 to 1024".to_string());
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
            dispatch_count: total,
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
