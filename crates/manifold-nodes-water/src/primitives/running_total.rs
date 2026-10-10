//! `node.running_total` — inclusive prefix sum of an `Array(u32)`, with the
//! grand total read back to the CPU one frame late. A barriered multi-pass
//! scan plus a readback bridge (ADDING_PRIMITIVES.md exclusions 1 and 3); the
//! scan is shared with `node.sort_particles_into_cells` (`prefix_scan.rs`) and
//! runs from `in` straight into `out`, no copy either side. Its
//! `extent` keeps last frame's total on the GPU (GPU_FLUID_SURFACE_DESIGN.md P6b).

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline};

use super::prefix_scan::PrefixScan;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TotalParams {
    n: u32,
    per_item: u32,
    _pad: [u32; 2],
}

/// `extent` words: the total, then Metal's three indirect-dispatch group counts.
const EXTENT_WORDS: u32 = 4;
/// Byte offset of the dispatch grid inside `extent`.
pub(crate) const EXTENT_GRID_OFFSET: u64 = 4;

manifold_node_engine::primitive! {
    name: RunningTotal,
    type_id: "node.running_total",
    purpose: "Inclusive running total of an Array<u32>: out[i] = in[0] + … + in[i] over the first `count` values (the whole array unwired). `total` is the grand total, read back to the CPU one frame late. `extent` holds the grand total on the GPU, then an indirect dispatch grid of 256-thread groups covering max(total, last frame's total) × per_item elements. With `capacity` set, a total past it is a named error every frame it lasts: the consumer that places the items holds only `capacity` of them.",
    inputs: {
        in: Array(u32) required,
        count: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
        total: ScalarF32,
        extent: Array(u32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("per_item"),
            label: "Elements Per Count",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("capacity"),
            label: "Consumer Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 16_777_216.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The building block for variable-length GPU output: per-item counts (triangles per cell, particles per bin) in, each item's inclusive end out, so a later atom finds its slot by binary search. Values past `count` are not written. `total` lags one frame; GPU consumers read the last scanned value directly instead. Wire `extent` to an emitter (node.volume_surface_mesh) so it dispatches over its live elements and clears last frame's; set per_item to its elements per count (3 vertices per triangle).",
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
        last_total: Option<GpuBuffer> = None,
        extent_scratch: Option<GpuBuffer> = None,
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
        match port {
            "out" => inputs.iter().find(|(name, _)| *name == "in").map(|&(_, n)| n),
            "extent" => Some(EXTENT_WORDS),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Last frame's GPU write, read before this frame's dispatch.
        if let Some(ptr) = self.total_cell.as_ref().and_then(GpuBuffer::mapped_ptr) {
            // SAFETY: a 4-byte shared buffer; only this node writes it.
            self.previous_total = unsafe { std::ptr::read(ptr.cast::<u32>()) } as f32;
        }
        ctx.outputs.set_scalar("total", ParamValue::Float(self.previous_total));
        if let Some(ParamValue::Float(capacity)) = ctx.params.get("capacity")
            && *capacity >= 1.0
            && self.previous_total > capacity.round()
        {
            let (needs, holds) = (self.previous_total as u64, capacity.round() as u64);
            ctx.error(format!("Running Total: needs {needs}, holds {holds}; {} items were dropped", needs - holds));
        }
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
            if self.last_total.is_none() {
                let last = gpu.device.create_buffer_shared(4);
                last.zero_fill();
                self.last_total = Some(last);
                self.extent_scratch = Some(gpu.device.create_buffer_shared(u64::from(EXTENT_WORDS) * 4));
            }
        }
        let (Some(input), Some(out)) = (ctx.inputs.array("in"), ctx.outputs.array("out")) else {
            return;
        };
        if requested.is_some_and(|count| !count.is_finite()) {
            ctx.error("Running Total: count must be finite");
            return;
        }
        let per_item = match ctx.params.get("per_item") {
            Some(ParamValue::Float(n)) => n.round().clamp(1.0, 64.0) as u32,
            _ => 1,
        };
        let extent = ctx
            .outputs
            .array("extent")
            .or(self.extent_scratch.as_ref())
            .expect("extent scratch allocated");
        let whole = (input.size / 4).min(out.size / 4);
        let n = requested.map_or(whole, |count| (count.max(0.0) as u64).min(whole)) as usize;
        let gpu = ctx.gpu_encoder();
        // Level 0 scans straight from `in` into `out`; only the block totals
        // live in the scan's own storage.
        let parents = match self.scan.parents(gpu.device, n) {
            Ok(buffer) => buffer.clone(),
            Err(error) => {
                ctx.error(format!("Running Total: {error}"));
                return;
            }
        };
        let encoder = &mut *gpu.native_enc;
        if n > 0 {
            self.scan.encode_into(encoder, n, input, out);
        }
        // With nothing scanned the total reads nothing; `out` may be empty.
        let scanned = if n > 0 { out } else { &parents };
        let uniforms = TotalParams { n: n as u32, per_item, _pad: [0; 2] };
        encoder.dispatch_compute(
            self.read_total.as_ref().expect("total pipeline created"),
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: scanned, offset: 0 },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: self.total_cell.as_ref().expect("total cell allocated"),
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: self.last_total.as_ref().expect("last total allocated"),
                    offset: 0,
                },
                GpuBinding::Buffer { binding: 4, buffer: extent, offset: 0 },
            ],
            [1, 1, 1],
            "node.running_total.total",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
