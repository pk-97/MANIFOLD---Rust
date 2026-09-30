//! `node.collar_source` — a collar vector's entries onto the lattice
//! (docs/FFT_WATER_SOLVER_DESIGN.md D3, D10): each collar cell takes its
//! entry's value times `scale`, added to `base` (zero unwired), ready for the
//! box solve. One thread per cell reading the running total, so no scatter. A
//! per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: `scale`, the derived `base_len`, then
/// `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SourceUniforms {
    scale: f32,
    base_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: CollarSource,
    type_id: "node.collar_source",
    purpose: "Put a collar vector on the lattice: out[c] = base[c] + scale · value[total[c] − 1] where the collar's inclusive running total steps up at cell c (c is the collar entry total[c] − 1), else base[c]. An unwired base is 0. The vector's last element is its constant and never lands on a cell; entries past the vector add nothing. One output per running-total element.",
    inputs: {
        total: Array(u32) required,
        value: Array(f32) required,
        base: Array(f32) optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("scale", "Scale", 1.0, -1e6, 1e6),
    ],
    depth_rule: Terminal,
    composition_notes: "The FFT water solve's operator, first half: collar_source → the 3D cosine box solve (cosine_reorder → fft_3d → cosine_spectrum → cosine_poisson_divide → cosine_half_spectrum → inverse_fft_3d → cosine_reorder) → node.collar_gather. total is node.running_total over node.collar_cells. A warm-started solve uses base and scale: the divergence minus the starting sources (base = divergence, scale −1), and the next solve's start (base = this solve's start, value = the solved rest, scale 1).",
    examples: [],
    picker: { label: "Collar Source", category: Atom },
    summary: "Places the pressure solver's values back on the grid cells at the water's edge.",
    category: Particles3D,
    role: Filter,
    aliases: ["scatter to grid", "collar to lattice"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/collar_source_body.wgsl"),
    input_access: [BufferGather, BufferGather, Coincident],
    // `base_len` gates the coincident base read: 0 unwired, where the fused
    // kernel passes a zero element and the standalone one binds `total`.
    derived_uniforms: ["base_len:u32"],
}

inventory::submit! {
    crate::node_graph::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.collar_source",
        array_ports: &["base"],
        recompute: |ctx| Some(vec![(ctx.array_len)("base").unwrap_or(0) as f32]),
    }
}

impl Primitive for CollarSource {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "total").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scale = ctx.scalar_or_param("scale", 1.0);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(total), Some(value), Some(out)) =
            (ctx.inputs.array("total"), ctx.inputs.array("value"), ctx.outputs.array("out"))
        else {
            return;
        };
        let base = ctx.inputs.array("base");
        let count = (total.size.min(out.size) / 4) as u32;
        if count == 0 || value.size < 4 {
            ctx.error("Collar Source: empty arrays".to_string());
            return;
        }
        // The standalone kernel reads base[idx] for every cell it writes.
        if base.is_some_and(|b| b.size < u64::from(count) * 4) {
            ctx.error("Collar Source: base is shorter than the lattice".to_string());
            return;
        }
        let uniforms = SourceUniforms {
            scale,
            base_len: base.map_or(0, |_| count),
            dispatch_count: count,
            _pad0: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: total, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: value, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: base.unwrap_or(total), offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.collar_source",
        );
    }
}
