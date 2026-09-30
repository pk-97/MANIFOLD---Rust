//! `node.cosine_half_spectrum` — the twiddle stage that starts an inverse 3D
//! cosine transform built on one real FFT (docs/FFT_WATER_SOLVER_DESIGN.md
//! D9). A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cosine_spectrum::{half_spectrum_len, lattice_nodes};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HalfSpectrumUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: CosineHalfSpectrum,
    type_id: "node.cosine_half_spectrum",
    purpose: "Inverse of node.cosine_spectrum: from 3D cosine-transform coefficients, rebuild the half spectrum (nx/2 + 1 entries along x) that node.inverse_fft_3d turns back into the reordered lattice. Eight gathers per entry. Lattice nodes_x/y/z, every length even.",
    inputs: {
        values: Array(f32) required,
    },
    outputs: {
        spectrum: Array([f32; 2]),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Nodes Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Nodes Z", 64.0, 2.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "cosine_half_spectrum → inverse_fft_3d → cosine_reorder (direction 1) undoes cosine_reorder → fft_3d → cosine_spectrum exactly (the inverse FFT carries the 1/N scale).",
    examples: [],
    picker: { label: "Cosine Half Spectrum", category: Atom },
    summary: "Starts turning cosine-wave strengths back into a 3D grid.",
    category: MathAndConvert,
    role: Map,
    aliases: ["idct", "dct-iii", "inverse cosine transform"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/cosine_half_spectrum_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for CosineHalfSpectrum {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "spectrum").then(|| lattice_nodes(params).map(half_spectrum_len)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = lattice_nodes(ctx.params) else {
            ctx.error("Cosine Half Spectrum: every length must be even, 2 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(spectrum)) = (ctx.inputs.array("values"), ctx.outputs.array("spectrum")) else {
            return;
        };
        let half = half_spectrum_len(nodes);
        if u64::from(nodes.iter().product::<u32>()) * 4 > values.size || u64::from(half) * 8 > spectrum.size {
            ctx.error(format!("Cosine Half Spectrum: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = HalfSpectrumUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: half,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: values, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: spectrum, offset: 0 },
            ],
            [half.div_ceil(256), 1, 1],
            "node.cosine_half_spectrum",
        );
    }
}
