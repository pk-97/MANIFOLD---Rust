//! Encode replay on Vulkan (design section 7, P1): a span encodes every
//! dispatch directly and the cache only carries stats, and a gated dispatch
//! is a plain indirect dispatch from its own gate. P6 records spans into
//! secondary command buffers.

use super::device::GpuDevice;
use super::encoder::GpuEncoder;
use super::types::{GpuBuffer, GpuComputePipeline};
use crate::GpuBinding;
use crate::replay::GpuReplayStats;

/// Recordings for one replay span. On Vulkan it holds no recordings yet.
#[derive(Default)]
pub struct GpuReplayCache {
    stats: GpuReplayStats,
}

impl GpuReplayCache {
    pub fn stats(&self) -> GpuReplayStats {
        self.stats
    }
}

impl GpuEncoder {
    /// Open a replay span; on Vulkan every dispatch encodes directly.
    pub fn begin_replay(&mut self, _device: &GpuDevice, cache: GpuReplayCache) {
        debug_assert!(self.replay.is_none(), "replay spans never nest");
        self.replay = Some(cache);
    }

    /// Close the span and hand the cache back.
    pub fn end_replay(&mut self) -> GpuReplayCache {
        self.replay.take().expect("end_replay without begin_replay")
    }

    /// Open a gated segment inside the span: the next `dispatch_compute_gated`
    /// calls, up to `commands` of them, run from one recording the GPU
    /// executes by `ranges[index]` (`GATED_RANGE_BYTES` per entry), written
    /// earlier in this command buffer as `{0, commands}` to run or `{0, 0}`
    /// to skip.
    pub fn begin_gated_segment(&mut self, _ranges: &GpuBuffer, _index: u32, _commands: u32) {
        // Every gated dispatch reads its own gate, so a segment holds no state.
    }

    /// Close the open gated segment; later dispatches are ungated.
    pub fn end_gated_segments(&mut self) {}

    /// A dispatch the GPU may have switched off: `gate` holds its indirect
    /// threadgroup counts at `gate_offset` (zeros when off), `groups` the
    /// counts it has when on.
    pub fn dispatch_compute_gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        _groups: [u32; 3],
        gate: &GpuBuffer,
        gate_offset: u64,
        label: &str,
    ) {
        self.dispatch_compute_indirect(pipeline, bindings, gate, gate_offset, label);
    }
}
