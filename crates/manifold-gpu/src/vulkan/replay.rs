//! Encode replay on Vulkan: not built. The twin of the Metal store is a
//! secondary command buffer per ring entry. Until the backend records
//! command buffers, a span encodes everything directly and the cache only
//! carries stats. Gated dispatches become plain indirect dispatches in P1
//! (design section 7).

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

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
    pub fn begin_gated_segment(&mut self, ranges: &GpuBuffer, index: u32, commands: u32) {
        todo!("vulkan P1")
    }

    /// Close the open gated segment; later dispatches are ungated.
    pub fn end_gated_segments(&mut self) {
        todo!("vulkan P1")
    }

    /// A dispatch the GPU may have switched off: `gate` holds its indirect
    /// threadgroup counts at `gate_offset` (zeros when off), `groups` the
    /// counts it has when on.
    pub fn dispatch_compute_gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        groups: [u32; 3],
        gate: &GpuBuffer,
        gate_offset: u64,
        label: &str,
    ) {
        todo!("vulkan P1")
    }
}
