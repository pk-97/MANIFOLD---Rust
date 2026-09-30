//! `node.collar_source` — a collar vector's entries onto the lattice
//! (docs/FFT_WATER_SOLVER_DESIGN.md D3, D10): each collar cell takes its
//! entry's value, every other cell zero, ready for the box solve. One thread
//! per cell reading the running total, so no scatter. A per-element gather on
//! the codegen path.

use manifold_gpu::GpuBinding;

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: no params, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SourceUniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CollarSource,
    type_id: "node.collar_source",
    purpose: "Put a collar vector on the lattice: out[c] = value[total[c] − 1] where the collar's inclusive running total steps up at cell c (c is the collar entry total[c] − 1), else 0. The vector's last element is its constant and never lands on a cell; entries past the vector give 0. One output per running-total element.",
    inputs: {
        total: Array(u32) required,
        value: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The FFT water solve's operator, first half: collar_source → the 3D cosine box solve (cosine_reorder → fft_3d → cosine_spectrum → cosine_poisson_divide → cosine_half_spectrum → inverse_fft_3d → cosine_reorder) → node.collar_gather. total is node.running_total over node.collar_cells.",
    examples: [],
    picker: { label: "Collar Source", category: Atom },
    summary: "Places the pressure solver's values back on the grid cells at the water's edge.",
    category: Particles3D,
    role: Filter,
    aliases: ["scatter to grid", "collar to lattice"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/collar_source_body.wgsl"),
    input_access: [BufferGather, BufferGather],
}

impl Primitive for CollarSource {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "total").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(total), Some(value), Some(out)) =
            (ctx.inputs.array("total"), ctx.inputs.array("value"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = (total.size.min(out.size) / 4) as u32;
        if count == 0 || value.size < 4 {
            ctx.error("Collar Source: empty arrays".to_string());
            return;
        }
        let uniforms = SourceUniforms { dispatch_count: count, _pad0: 0, _pad1: 0, _pad2: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: total, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: value, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.collar_source",
        );
    }
}
