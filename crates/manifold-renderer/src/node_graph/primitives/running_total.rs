//! `node.running_total` — inclusive prefix sum of an `Array(u32)`, with the
//! grand total read back to the CPU one frame late. A barriered multi-pass
//! scan plus a readback bridge (ADDING_PRIMITIVES.md exclusions 1 and 3); the
//! scan is shared with `node.sort_particles_into_cells` (`prefix_scan.rs`).

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::prefix_scan::PrefixScan;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TotalParams {
    n: u32,
    _pad: [u32; 3],
}

crate::primitive! {
    name: RunningTotal,
    type_id: "node.running_total",
    purpose: "Inclusive running total of an Array<u32>: out[i] = in[0] + … + in[i] over the first `count` values (the whole array unwired). `total` is the grand total, read back to the CPU one frame late.",
    inputs: {
        in: Array(u32) required,
        count: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
        total: ScalarF32,
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "The building block for variable-length GPU output: per-item counts (triangles per cell, particles per bin) in, each item's inclusive end out, so a later atom finds its slot by binary search. Values past `count` are not written. `total` lags one frame; GPU consumers read the last scanned value directly instead.",
    examples: [],
    picker: { label: "Running Total", category: Atom },
    summary: "Adds up a list of counts as it goes, so each item knows where its results start.",
    category: MathAndConvert,
    role: Filter,
    aliases: ["prefix sum", "scan", "cumulative sum", "running sum"],
    boundary_reason: BarrieredReduction,
    extra_fields: {
        scan: PrefixScan = PrefixScan::default(),
        read_total: Option<GpuComputePipeline> = None,
        total_cell: Option<GpuBuffer> = None,
        previous_total: f32 = 0.0,
    },
}

impl Primitive for RunningTotal {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| inputs.iter().find(|(name, _)| *name == "in").map(|&(_, n)| n))
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Last frame's GPU write, read before this frame's dispatch.
        if let Some(ptr) = self.total_cell.as_ref().and_then(GpuBuffer::mapped_ptr) {
            // SAFETY: a 4-byte shared buffer; only this node writes it.
            self.previous_total = unsafe { std::ptr::read(ptr.cast::<u32>()) } as f32;
        }
        ctx.outputs.set_scalar("total", ParamValue::Float(self.previous_total));
        // Unwired, the whole array: a numeric default would cap it at a float's
        // exact-integer range.
        let requested = match ctx.inputs.scalar("count") {
            Some(ParamValue::Float(count)) => Some(count),
            _ => None,
        };
        {
            let gpu = ctx.gpu_encoder();
            self.scan.prepare(gpu.device);
            if self.read_total.is_none() {
                self.read_total = Some(gpu.device.create_compute_pipeline(
                    include_str!("shaders/running_total.wgsl"),
                    "read_total",
                    "node.running_total",
                ));
            }
            if self.total_cell.is_none() {
                self.total_cell = Some(gpu.device.create_buffer_shared(4));
            }
        }
        let (Some(input), Some(out)) = (ctx.inputs.array("in"), ctx.outputs.array("out")) else {
            return;
        };
        if requested.is_some_and(|count| !count.is_finite()) {
            ctx.error("Running Total: count must be finite");
            return;
        }
        let whole = (input.size / 4).min(out.size / 4);
        let n = requested.map_or(whole, |count| (count.max(0.0) as u64).min(whole)) as usize;
        let gpu = ctx.gpu_encoder();
        let values = match self.scan.buffer(gpu.device, n) {
            Ok(buffer) => buffer.clone(),
            Err(error) => {
                ctx.error(format!("Running Total: {error}"));
                return;
            }
        };
        let bytes = (n * 4) as u64;
        let encoder = &mut *gpu.native_enc;
        if bytes > 0 {
            encoder.copy_buffer_to_buffer(input, &values, bytes);
        }
        self.scan.encode(encoder, n);
        if bytes > 0 {
            encoder.copy_buffer_to_buffer(&values, out, bytes);
        }
        let uniforms = TotalParams { n: n as u32, _pad: [0; 3] };
        encoder.dispatch_compute(
            self.read_total.as_ref().expect("total pipeline created"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &values, offset: 0 },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: self.total_cell.as_ref().expect("total cell allocated"),
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.running_total.total",
        );
    }
}
