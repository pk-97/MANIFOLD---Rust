//! `node.select_flagged` — the list of flagged items from an inclusive
//! running total (docs/FFT_WATER_SOLVER_DESIGN.md D6): entry e is the item
//! where the total first passes e, found by binary search. The collar
//! compaction's place step, one thread per entry, no scatter. A per-element
//! gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::int_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SelectUniforms {
    capacity: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

pub(super) fn capacity(params: &ParamValues) -> u32 {
    match params.get("capacity") {
        Some(ParamValue::Float(v)) => v.round().clamp(1.0, 16_777_216.0) as u32,
        _ => 32_768,
    }
}

crate::primitive! {
    name: SelectFlagged,
    type_id: "node.select_flagged",
    purpose: "List the flagged items in order from an inclusive running total of 0/1 flags (node.running_total): out[e] is the first index whose total exceeds e, for e < capacity. Entries past the grand total hold 4294967295 (no item). Items past `capacity` are dropped.",
    inputs: {
        total: Array(u32) required,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        int_param!("capacity", "Capacity", 32_768.0, 1.0, 16_777_216.0),
    ],
    depth_rule: Terminal,
    composition_notes: "node.collar_cells → node.running_total → select_flagged compacts the air collar into a list of cells, in cell order, without atomics. Size capacity for the largest collar the scene makes; the running total's `total` scalar (one frame late) says how full it is.",
    examples: [],
    picker: { label: "Select Flagged", category: Atom },
    summary: "Turns a grid of yes/no flags into a short list of the flagged cells.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["compact", "stream compaction", "gather flagged", "index list"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/select_flagged_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for SelectFlagged {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| capacity(params))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let entries = capacity(ctx.params);
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(total), Some(out)) = (ctx.inputs.array("total"), ctx.outputs.array("out")) else {
            return;
        };
        if u64::from(entries) * 4 > out.size || total.size < 4 {
            ctx.error(format!("Select Flagged: {entries} entries do not fit its arrays"));
            return;
        }
        let uniforms = SelectUniforms { capacity: entries as i32, dispatch_count: entries, _pad0: 0, _pad1: 0 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: total, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [entries.div_ceil(256), 1, 1],
            "node.select_flagged",
        );
    }
}
