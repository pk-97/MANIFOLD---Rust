//! Encode replay on Vulkan: not built. The twin of the Metal store is a
//! secondary command buffer per ring entry (`docs/ENCODE_REPLAY_DESIGN.md`
//! D10). Until the backend records command buffers, a span encodes
//! everything directly and the cache only carries stats.

use super::device::GpuDevice;
use super::encoder::GpuEncoder;
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
}
